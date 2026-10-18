//! `CHudTSStatus` (0x100d43cc) drawing: round timer, TS messages, objective text, teammate
//! markers, crosshair ID, status panel, power-up, objective markers (§4.3, `Draw` 10049ea0).

use super::Dc;
use super::draw::{GREEN, RED, ftol, half, num};
use super::state::Hud;
use super::text::localise;
use crate::input;
use goldsrc::Global;
use goldsrc::client::engine;
use goldsrc::sys::HSPRITE;
use std::cell::Cell;

/// `sprites/slowmotion.spr`, loaded the first time a ruleset shows the slow-motion icon
/// ([`draw_slowmo_icon`]), so TS 3.0's sprite list stays as its own. Sprite handles die with the
/// video mode, so [`vid_init`] forgets it.
static SLOWMO_SPR: Global<Cell<Option<HSPRITE>>> = Global(Cell::new(None));

/// Forget the sprite handles this module loads on demand (`HUD_VidInit`).
pub fn vid_init() {
    SLOWMO_SPR.0.set(None);
}

/// The voice helper's lookup (`GetPlayerTextColor` 1001fcd0): negative teams are 0, then modulo
/// into `iTeamColors` (`0x10096974`, the scoreboard's table: (255,170,0), (125,165,210),
/// (200,90,70), (225,205,45), (145,215,140)), which the status panel's team bar uses too.
pub fn team_color(team: i32) -> [i32; 3] {
    let t = crate::menus::widgets::TEAM_COLORS;
    t[team.max(0) as usize % t.len()].map(i32::from)
}

pub fn draw(dc: &Dc, nohud: bool) {
    if nohud {
        return;
    }
    let dead_or_obs = super::dead_or_observer();
    let id = if dead_or_obs { None } else { crosshair_trace() };
    // Port addition: the client extensions' HUD control (issue #314, `super::elements`).
    use super::elements::draw as element;
    use ts_client_ext_api::hud as el;
    super::with(|h| {
        h.crosshair_id = id.as_ref().map(|(i, t, _)| (*i, *t));
        if h.round_time != 0.0 {
            element(el::TIMER, || draw_round_timer(dc, h));
        }
        element(el::NOTIFY, || draw_ts_messages(dc, h));
        element(el::OBJECTIVE, || draw_flag_panel(dc, h));
        if dead_or_obs {
            return;
        }
        if h.gamemode != 0 {
            draw_teammate_markers(dc, h);
        }
        if let Some((_, team, name)) = &id {
            element(el::CROSSHAIR, || dc.string(dc.w() / 2, dc.h() / 2 - 40, name, if *team == 1 { GREEN } else { RED }));
        }
        draw_status_panel(dc, h);
        if h.pw_flags == 0 {
            h.pw_expire = 0.0;
        } else {
            element(el::POWERUP, || draw_powerup(dc, h));
        }
        if h.gamemode > 1 {
            element(el::OBJECTIVE, || draw_objective_markers(dc, h));
        }
    });
}

/// `DrawRoundTimer` (1004a130): top-right `MM:SS`, red under a minute, else the HUD colour × 0.5
/// (`0x1004a1c5`).
fn draw_round_timer(dc: &Dc, h: &mut Hud) {
    let secs = h.tick_round_timer(dc.now);
    let rgb = if secs < 60 { RED } else { half(dc.hud) };
    let mut x = dc.w() - 0x48;
    x = dc.number(x, 0xc, num::DIGITS_2 | num::DRAW_ZERO, secs / 60, rgb);
    x = dc.string(x, 12, ":", rgb);
    dc.number(x, 12, num::DIGITS_2 | num::DRAW_ZERO | num::LEADING_ZEROS, secs % 60, rgb);
}

/// `DrawTSMessages` (1004af40): timed messages, capped at 10 s, fading over the last second.
/// Black (0,0,0) means "HUD colour".
fn draw_ts_messages(dc: &Dc, h: &mut Hud) {
    let build = crate::gameplay::hud_rules(h.gameplay).dll_build();
    for m in h.messages.iter_mut() {
        if m.expire <= dc.now {
            continue;
        }
        // `expire - now > 10.0f` on the x87, unrounded (`0x1004af74`, 2.1 `0x10086f1f`).
        if m.expire as f64 - dc.now as f64 > 10.0 {
            m.expire = dc.now + 10.0;
        }
        // `0x1004afc9..0x1004b085`: each channel is `__ftol(base * min(expire - now, 1))`, the
        // base being the message colour, or for a black message the `hud_r/g/b` cvar values
        // themselves (floats, not rounded first).
        let ic = crate::input::cvars();
        let base = if m.rgb == [0; 3] { [ic.hud_r.value(), ic.hud_g.value(), ic.hud_b.value()] } else { m.rgb.map(|c| c as f32) };
        let rgb = base.map(|c| ts_message_channel(build, c, m.expire, dc.now));
        dc.string(m.x, m.y, &m.text, rgb);
    }
}

/// One `DrawTSMessages` colour channel: `__ftol(base × min(expire − now, 1))`. 3.0 keeps the
/// difference and the product on the x87 (`0x1004afe3..0x1004b003`); 2.1 stores the factor to a
/// float first (`0x1008700d`, or 1.0 at `0x10087012`), then multiplies on the x87.
fn ts_message_channel(build: crate::gameplay::DllBuild, base: f32, expire: f32, now: f32) -> i32 {
    let diff = expire as f64 - now as f64;
    let f = match build {
        crate::gameplay::DllBuild::Optimised => diff.min(1.0),
        crate::gameplay::DllBuild::Debug => {
            if diff < 1.0 {
                diff as f32 as f64
            } else {
                1.0
            }
        }
    };
    (base as f64 * f) as i32
}

/// `DrawHudStringLen(0, max, text)` (`0x10021ca0`): the width up to the first character that
/// would pass `max`.
fn string_len_to(dc: &Dc, max: i32, text: &str) -> i32 {
    let mut x = 0i32;
    for c in text.chars().take_while(|&c| c != '\n') {
        let w = dc.g.screen.char_widths[(c as u32 & 0xff) as usize] as i32;
        if x.wrapping_add(w) > max {
            break;
        }
        x = x.wrapping_add(w);
    }
    x
}

/// The flag panel (`0x1004b0e0`, run while `TSFlag` has set the count): two blink timers tick
/// by frame time × time scale (0.1 s and 0.5 s, each toggling a phase); each of the first
/// `count` (≤ 16) flags with a state and a team 1..4 draws the flag sprite at
/// `(w − 56, team·48 + 40)` in the team colour (`iTeamColors`) × (1 or 0.5 by the fast phase
/// while carried, else × 0.75 × (1 or 0.5 by the slow phase)). A carrier 1..64 gets their name
/// (24 bytes) right-aligned against the flag, red (175, 40, 40) for the other team, green
/// (40, 175, 40) for yours; if the carrier is you, "Take the Briefcase to the base!" at
/// y 320, right-aligned 32 px from the edge, in the HUD colour with 35 more red. The count
/// becomes one past the last flag drawn.
fn draw_flag_panel(dc: &Dc, h: &mut Hud) {
    if h.flag_count < 1 {
        return;
    }
    let hud = dc.hud;
    let e = engine();
    let local = e.local_player().map_or(0, |p| p.index);
    let team_of = |h: &Hud, i: i32| usize::try_from(i).ok().and_then(|i| h.players.get(i)).map_or(0, |p| p.team);
    let my_team = team_of(h, local);
    // `m_flTimeDelta / (1 / max(ts, 0.01))`.
    let ts = crate::state::with(|s| s.time_scale).max(0.01);
    let step = super::time_delta() as f64 * ts as f64;
    for (blink, reset) in h.flag_blink.iter_mut().zip([0.1f32, 0.5]) {
        blink.0 = (blink.0 as f64 - step) as f32;
        if blink.0 < 0.0 {
            blink.0 = reset;
            blink.1 = !blink.1;
        }
    }
    let (fast, slow) = (h.flag_blink[0].1, h.flag_blink[1].1);
    let n = h.flag_count.min(16) as usize;
    h.flag_count = 0;
    let (w, x) = (dc.w(), dc.w() - 0x38);
    for i in 0..n {
        let f = h.flags[i];
        if f.state == 0 || !(1..=4).contains(&f.team) {
            continue;
        }
        let y = f.team as i32 * 0x30 + 0x28;
        let k: f64 = if f.state == 1 { if fast { 1.0 } else { 0.5 } } else { (if slow { 1.0 } else { 0.5 }) * 0.75 };
        let c = crate::menus::widgets::TEAM_COLORS[f.team as usize].map(|v| ftol(v as f64 * k));
        dc.spr(dc.g.ts.flag, x, y, c);
        let carrier = f.carrier as i32;
        if (1..=64).contains(&carrier) {
            if carrier == local {
                let msg = "Take the Briefcase to the base!";
                let rgb = [(hud[0] + 0x23).min(0xff), hud[1], hud[2]];
                super::text::draw_string(&dc.g.screen, w - string_len_to(dc, w, msg) - 0x20, 0x140, w, msg, rgb);
            }
            let mut name = e.player_info(carrier).name_bytes;
            name.truncate(0x18);
            let name = super::saytext::string(&name);
            // `0x1004b3b6..`: (195, 45) or (45, 195) × 0.9f, blue 40.
            let rgb = if team_of(h, carrier) != my_team { [175, 40, 40] } else { [40, 175, 40] };
            super::text::draw_string(&dc.g.screen, x - string_len_to(dc, w, &name), y, w, &name, rgb);
        }
        h.flag_count = i as i32 + 1;
    }
}

/// `GetPlayerScreenPos` (1004b470): `WorldToScreen(origin + 40)`.
fn player_screen_pos(dc: &Dc, index: i32) -> Option<(i32, i32)> {
    let e = engine();
    let ent = e.entity(index)?;
    let o = ent.origin;
    e.world_to_screen([o[0], o[1], o[2] + 40.0], &dc.g.screen)
}

/// `DrawTeammateMarkers` (1004ab40): a green dot over visible teammates. TS also has a branch
/// that draws the flag sprite over carriers, but its test reads a `g_PlayerExtraInfo` field
/// nothing writes (`0x101901f0`, only zeroed at `0x1005d5c0`), so it never runs. TS's
/// visibility flags come from `HUD_AddEntity`; the port uses "updated in the last packet".
fn draw_teammate_markers(dc: &Dc, h: &Hud) {
    let e = engine();
    let Some(me) = e.local_player() else { return };
    let my_team = h.players.get(me.index as usize).map_or(0, |p| p.team);
    for i in 1..=e.max_clients().min(64) {
        if i == me.index {
            continue;
        }
        let Some(ent) = e.entity(i) else { continue };
        if ent.player == 0 || ent.curstate.messagenum != me.curstate.messagenum {
            continue;
        }
        let team = h.players[i as usize].team;
        let Some((x, y)) = player_screen_pos(dc, i) else { continue };
        if team == my_team {
            dc.fill(x - 2, y - 2, 4, 4, GREEN, 0x75);
        }
    }
}

/// `DrawCrosshairID` (1004acc0): player trace 8192 units along the view. Returns
/// (index, 1 = teammate / 2 = enemy, name).
fn crosshair_trace() -> Option<(i32, u8, String)> {
    let e = engine();
    let me = e.local_player()?;
    // `GetCullOrigin` (`0x10057270`: the eye in third person, else `v_origin`) along the last
    // refdef's forward (`0x1018bd90`, punch and bob included).
    let start = if crate::view::is_third_person() { crate::view::eye() } else { crate::view::origin() };
    let f = crate::view::camera().forward;
    let end = [start[0] + f[0] * 8192.0, start[1] + f[1] * 8192.0, start[2] + f[2] * 8192.0];
    let (_, _, idx) = e.crosshair_trace(start, end, me.index)?;
    let ent = e.entity(idx)?;
    if ent.player == 0 || !(1..=64).contains(&idx) {
        return None;
    }
    let name = super::saytext::string(&e.player_info(idx).name_bytes);
    if name.is_empty() {
        return None;
    }
    let team = |h: &Hud, i: i32| usize::try_from(i).ok().and_then(|i| h.players.get(i)).map_or(0, |p| p.team);
    let (gm, mine, theirs) = super::with(|h| (h.gamemode, team(h, me.index), team(h, idx)));
    let team = if gm == 0 || mine != theirs { 2 } else { 1 };
    Some((idx, team, name))
}

/// `DrawStatusPanel` (1004a3e0): bottom-left box with the stance sprite filled bottom-up by
/// health, health number, health/kevlar icons, team bar, free slots, credits, kung-fu and
/// movement icons. Hidden by `cl_realhud 1`.
fn draw_status_panel(dc: &Dc, h: &Hud) {
    if input::cvars().cl_realhud.value() != 0.0 {
        return;
    }
    use super::elements::draw as element;
    use ts_client_ext_api::hud as el;
    let e = engine();
    let look = crate::gameplay::hud_rules(h.gameplay).status_look();
    let hud = dc.hud;
    let (w, ht) = (dc.w(), dc.h());
    let y = ht - 0x30;
    element(el::HEALTH, || {
        dc.fill(8, y, 0x7f, 0x27, hud, 0xf);
        let stance = dc.g.ts.stance[(h.stance as usize).min(2)];
        if stance != 0 && !look.stance_health_fill {
            e.spr_set(stance, half(hud));
            e.spr_draw_additive(0, 8, y - 4, None);
        } else if stance != 0 {
            let sh = e.spr_height(stance, 0);
            let sw = e.spr_width(stance, 0);
            let hp = (h.health as i32).min(100);
            // C int arithmetic: the sprite's height is file data.
            let top = sh.wrapping_sub(hp.wrapping_mul(sh) / 100);
            if h.health < 100 {
                // Red over the part health doesn't fill (`0x1004a4d5..0x1004a518`: rows 0..top).
                e.spr_set(stance, RED);
                e.spr_draw_additive(0, 8, y - 4, Some(&goldsrc::sys::wrect_t { left: 0, right: sw, top: 0, bottom: top }));
            }
            let rc = goldsrc::sys::wrect_t { left: 0, right: sw, top, bottom: sh };
            e.spr_set(stance, half(hud));
            e.spr_draw_additive(0, 8, (y - 4).wrapping_add(top), Some(&rc));
        }
        let hp_rgb = if h.health < 50 { RED } else { half(hud) };
        dc.number(0x48, y + 4, num::DIGITS_3 | num::DRAW_ZERO, h.health as i32, hp_rgb);
        dc.spr(dc.g.ts.health, 0x68, y, hp_rgb);
    });
    if h.armor != 0 {
        element(el::ARMOUR, || {
            let rgb = if h.armor < 50 { RED } else { half(hud) };
            if look.armor_number {
                dc.number(0x48, y + 0x18, num::DIGITS_3 | num::DRAW_ZERO, h.armor as i32, rgb);
                dc.spr(dc.g.ts.kevlar, 0x68, y + 0x14, rgb);
            } else {
                dc.spr(dc.g.ts.kevlar, 0x48, y + 0x14, rgb);
            }
        });
    }
    // Team bar (teamplay): part of the health box. It may scale the colour the rest uses.
    let hud = element(el::HEALTH, || {
        let mut hud = hud;
        let y = y - 0x14;
        if h.gamemode != 0
            && let Some(me) = e.local_player()
            && let Some(p) = usize::try_from(me.index).ok().and_then(|i| h.players.get(i))
        {
            // `iTeamColors[team]` (`0x1004a748`); guarded: TS indexes past the five entries.
            if let Some(a) = look.team_bar_scale {
                dc.fill(8, y, 0x7f, 0x13, hud, 0x23);
                hud = scale_colors_float(hud, a);
                dc.string(10, y, &p.teamname, hud);
            } else {
                let t = crate::menus::widgets::TEAM_COLORS;
                let c = t[(p.team.max(0) as usize).min(t.len() - 1)].map(i32::from);
                dc.fill(8, y, 0x7f, 0x13, c, 0x23);
                dc.string(10, y, &p.teamname, c);
            }
        }
        hud
    });
    // Free slots and credits.
    element(el::CASH, || {
        let mut y = ht - 0x1c;
        let x = 0x88;
        dc.fill(x, y, 0x7f, 0x13, hud, 0xf);
        let rgb = if h.slots == 0 { RED } else { half(hud) };
        let nx = dc.number(x + 8, y + 2, num::DIGITS_2 | num::DRAW_ZERO, h.slots as i32, rgb);
        let (slots_label, credits_label) =
            look.labels.map_or_else(|| (localise("#HUD_FreeSlots"), localise("#HUD_Credits")), |(s, c)| (s.to_owned(), c.to_owned()));
        dc.string(nx + 8, y, &slots_label, rgb);
        if h.gamemode > 1 || h.cashbased != 0 {
            y -= 0x14;
            dc.fill(x, y, 0x7f, 0x13, hud, 0xf);
            let rgb = if h.cash < 1000 { RED } else { half(hud) };
            let nx = dc.number(x, y + 2, num::DIGITS_5 | num::DRAW_ZERO, h.cash, rgb);
            dc.string(nx + 8, y, &credits_label, rgb);
        }
    });
    // `0x1004aa4e`, `0x1004aad2`: the HUD colour × 2/3.
    let icon = if look.icons_divide { hud.map(|c| ftol(c as f64 / 1.5)) } else { hud.map(|c| ftol(c as f64 * 0.666_666_666_666_666_6)) };
    if h.kungfu_icon {
        element(el::KUNGFU, || dc.spr(dc.g.ts.kungfu, w / 2 - 0x30, ht - 0x38, icon));
    }
    if h.move_icon {
        element(el::STUNT, || dc.spr(dc.g.ts.movement, w / 2, ht - 0x38, icon));
    }
    if crate::gameplay::hud_rules(h.gameplay).slowmo_icon() {
        // 2.1 reads the panel's colour, scaled by the team bar.
        element(el::POWERUP, || draw_slowmo_icon(dc, hud));
    }
}

/// The slow-motion icon, for a ruleset that has one
/// ([`ClientRules::slowmo_icon`](crate::gameplay::ClientRules::slowmo_icon)): while the last
/// `TSSlowMo` is non-zero, `sprites/slowmotion.spr` additive at (w / 2 − 16, 16) in the HUD colour
/// / 1.5, truncated. TS 2.1's status `Draw` tail (`0x10086651`; its other trigger, the
/// `GloSlow` flag, is never sent).
fn draw_slowmo_icon(dc: &Dc, hud: [i32; 3]) {
    if crate::state::with(|s| s.slowmo_factor100) == 0.0 {
        return;
    }
    let e = engine();
    let spr = SLOWMO_SPR.0.get().unwrap_or_else(|| {
        let spr = e.spr_load("sprites/slowmotion.spr");
        SLOWMO_SPR.0.set(Some(spr));
        spr
    });
    if spr == 0 {
        return;
    }
    e.spr_set(spr, slowmo_icon_rgb(hud));
    e.spr_draw_additive(0, dc.w() / 2 - 0x10, 0x10, None);
}

/// `ScaleColors(r, g, b, a)` as TS 2.1 has it (`0x10047e90`): `k = (float)(a / 255.0f)`, then
/// `ftol(c × k)` per channel.
pub(crate) fn scale_colors_float(c: [i32; 3], a: i32) -> [i32; 3] {
    let k = (a as f64 / 255.0) as f32 as f64;
    c.map(|v| ftol(v as f64 * k))
}

/// The slow-motion icon's colour: each HUD channel / 1.5, truncated (`0x1008668f`).
fn slowmo_icon_rgb(hud: [i32; 3]) -> [i32; 3] {
    hud.map(|c| ftol(c as f64 / 1.5))
}

/// `DrawPowerup` (1004a2e0): top-left power-up name and seconds left (green).
/// The name lives in the static buffer `0x1018a5e0`, rewritten only for flags 1, 2, 8 and 0x10
/// (`0x1004a32e` jumps straight to the draw otherwise), so any other bit shows the last name
/// again (`Hud::pw_label`, which nothing else clears, as nothing clears the buffer).
fn draw_powerup(dc: &Dc, h: &mut Hud) {
    let f = h.pw_flags;
    let label = [(1, "#Pwr_SlowMotion"), (2, "#Pwr_InfiniteAmmo"), (8, "#Pwr_SlowPause"), (0x10, "#Pwr_DoubleFireRate")]
        .into_iter()
        .find(|&(bit, _)| f & bit != 0);
    if let Some((bit, key)) = label {
        // A ruleset may write the name out instead (`ClientRules::powerup_label`).
        h.pw_label = crate::gameplay::hud_rules(h.gameplay).powerup_label(bit).map_or_else(|| localise(key), str::to_owned);
    }
    let x = dc.string(8, 8, &h.pw_label, GREEN);
    // `0x1004a38b..0x1004a3d3`: the seconds left while timed (0 included), else the stored
    // value when it isn't 0; flags 3.
    let n = if h.pw_expire > dc.now { Some(ftol(h.pw_expire as f64 - dc.now as f64)) } else { (h.pw_value != 0).then_some(h.pw_value) };
    if let Some(n) = n {
        dc.number(x + 4, 8, num::DIGITS_2 | num::DRAW_ZERO, n, GREEN);
    }
}

/// `DrawObjectiveMarkers` (10049fc0; 2.1 `0x10085700`): compass sprite at each visible
/// objective, up/down arrow when it's more than 72 units above/below `gHUD.m_vecOrigin`'s z
/// (`0x100cf960`; 2.1 `0x102da408`), and the label.
fn draw_objective_markers(dc: &Dc, h: &Hud) {
    let e = engine();
    let c = [100, 100, 205];
    for o in h.objectives.iter().take(h.obj_max.max(0) as usize + 1) {
        if !o.visible || o.label.is_empty() {
            continue;
        }
        let Some((x, y)) = e.world_to_screen(o.pos, &dc.g.screen) else { continue };
        dc.spr(dc.g.ts.compass, x - 16, y - 16, c);
        match objective_arrow(o.pos[2], h.vec_origin) {
            Some(true) => dc.spr(dc.g.ts.uparrow, x - 16, y - 0x30, c),
            Some(false) => dc.spr(dc.g.ts.downarrow, x - 16, y + 16, c),
            None => {}
        }
        // Drawn as stored: TS doesn't localise the label (`0x10049fc0`).
        dc.string(x + 16, y - 8, &o.label, c);
    }
}

/// The objective marker's arrow for an objective at height `z`: up (`Some(true)`) more than
/// 72 units above `gHUD.m_vecOrigin` (`origin`), down below it, else none. `z ± 72.0f` stays on
/// the x87, unrounded, in both rulesets (`0x1004a05c`, 2.1 `0x100857e0`).
fn objective_arrow(z: f32, origin: [f32; 3]) -> Option<bool> {
    let my_z = origin[2] as f64;
    if z as f64 > my_z + 72.0 {
        Some(true)
    } else if (z as f64) < my_z - 72.0 {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2.1 stores `min(expire − now, 1)` to a float (`0x1008700d`) before `base ×` it: early in a
    /// map the difference needs more than a float's bits, and the channel can come out a step
    /// apart.
    #[test]
    fn ts_message_fade_factor_is_a_float_in_2_1() {
        use crate::gameplay::DllBuild;
        let (now, expire) = (f32::from_bits(0x3efc_8d11), f32::from_bits(0x3f8d_f213));
        assert_eq!(ts_message_channel(DllBuild::Optimised, 255.0, expire, now), 156);
        assert_eq!(ts_message_channel(DllBuild::Debug, 255.0, expire, now), 157);
        assert_eq!(ts_message_channel(DllBuild::Debug, 255.0, now + 2.0, now), 255);
    }

    /// `DrawObjectiveMarkers` measures from `gHUD.m_vecOrigin` (`0x100cf960`), the client data
    /// origin, not the local entity's.
    #[test]
    fn objective_arrow_uses_the_client_data_origin() {
        let origin = [0.0, 0.0, 100.0];
        assert_eq!(objective_arrow(172.0, origin), None);
        assert_eq!(objective_arrow(172.5, origin), Some(true));
        assert_eq!(objective_arrow(28.0, origin), None);
        assert_eq!(objective_arrow(27.5, origin), Some(false));
    }

    /// TS 2.1's `ScaleColors` (`0x10047e90`) rounds `a / 255` to a `float` first.
    #[test]
    fn scale_colors_rounds_the_factor() {
        assert_eq!(scale_colors_float([255, 160, 100], 200), [200, 125, 78]);
        assert_eq!(scale_colors_float([1, 0, 255], 255), [1, 0, 255]);
    }
}
