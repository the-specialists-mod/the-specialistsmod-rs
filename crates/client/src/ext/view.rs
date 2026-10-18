//! The read-only game state client extensions get (docs/implementation/client-ext-api.md §6): what
//! the player can see, from the HUD's state, the scoreboard and the view. Every query copies into
//! the API's `#[repr(C)]` structs; nothing hands out engine memory.
//!
//! The rule is the screen's: nothing here says where a player is unless the player is in view
//! ([`visible_players`]), and the only trace through players starts at the eye ([`aim`]).

use super::events::fill;
use goldsrc::client::engine;
use goldsrc::ext::render;
use goldsrc::sys::EF_NODRAW;
use ts_client_ext_api::{AimInfo, LocalState, NOT_FOUND, PlayerScore, TraceInfo, UNAVAILABLE, ViewInfo, VisiblePlayer, hit, local, player};

/// How far [`aim`] traces (the crosshair ID's range).
pub const AIM_RANGE: f32 = 8192.0;
/// `EV_PlayerTrace` flag: the world only (SDK `pm_defs.h` `PM_WORLD_ONLY`).
const PM_WORLD_ONLY: i32 = goldsrc::sys::PM_WORLD_ONLY as i32;
/// Above a player's origin (the hull's centre), where [`visible_players`] looks for its head.
const HEAD_HEIGHT: f32 = 24.0;

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn len(a: [f32; 3]) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// The local player's entity index, 0 for none.
fn local_index() -> i32 {
    engine().local_player().map_or(0, |p| p.index)
}

/// The eye: the HUD's predicted origin plus the view offset.
fn eye() -> [f32; 3] {
    let origin = crate::hud::with(|h| h.vec_origin);
    add(origin, engine().local_view_height())
}

/// The local team number (`g_iTeamNumber`).
fn local_team() -> i32 {
    crate::state::with(|s| s.team_number)
}

/// On-ground as the last `V_CalcRefdef` saw it ([`note_refdef`]).
static ON_GROUND: goldsrc::Global<std::cell::Cell<bool>> = goldsrc::Global(std::cell::Cell::new(false));

/// `V_CalcRefdef`'s `onground`, for [`LocalState::flags`].
pub fn note_refdef(on_ground: bool) {
    ON_GROUND.0.set(on_ground);
}

/// Ducked (`FL_DUCKING`) as the last `HUD_PostRunCmd` predicted it ([`note_ducking`]).
static DUCKING: goldsrc::Global<std::cell::Cell<bool>> = goldsrc::Global(std::cell::Cell::new(false));

/// `HUD_PostRunCmd`'s `to.client.flags & FL_DUCKING`, for [`LocalState::flags`] (issue #306).
/// `FL_DUCKING` is set once the duck has finished, as the server's movement sets it (the hull is
/// the ducked one), not while the view is still lowering.
pub fn note_ducking(ducking: bool) {
    DUCKING.0.set(ducking);
}

/// [`LocalState::flags`] from the state each comes from.
pub fn local_flags(on_ground: bool, ducking: bool, observer: bool, intermission: bool) -> u32 {
    let mut flags = 0;
    for (on, bit) in
        [(on_ground, local::ON_GROUND), (observer, local::OBSERVER), (intermission, local::INTERMISSION), (ducking, local::DUCKING)]
    {
        if on {
            flags |= bit;
        }
    }
    flags
}

/// `ClientExtApi::local_state`.
pub fn local_state() -> Result<LocalState, i32> {
    let e = engine();
    let index = local_index();
    if index <= 0 {
        return Err(NOT_FOUND);
    }
    let (health, armor, weapon, clip, ammo, observer, intermission) = crate::hud::with(|h| {
        let ammo = shared::weapons::table::tables().weapon(h.weapon as u8).map_or(0, |w| h.reserve[w.ammo as usize & 15]);
        (h.last_hp, h.armor as i32, h.weapon, h.clip, ammo, h.observer, h.intermission)
    });
    let angles = e.view_angles();
    let velocity = crate::state::with(|s| s.local_velocity);
    // An observer's or a dead player's last predicted duck isn't theirs now.
    let flags = local_flags(ON_GROUND.0.get(), DUCKING.0.get() && health > 0 && !observer, observer, intermission);
    Ok(LocalState {
        index,
        alive: (health > 0 && !observer) as i32,
        team: local_team(),
        health,
        armor,
        weapon,
        clip,
        ammo,
        flags,
        eye: eye(),
        angles,
        forward: e.angle_vectors(angles).0,
        velocity,
        speed: len(velocity),
    })
}

/// `ClientExtApi::player`: one scoreboard row.
pub fn player_score(slot: i32) -> Result<PlayerScore, i32> {
    let e = engine();
    if slot < 1 || slot > e.max_clients() {
        return Err(NOT_FOUND);
    }
    let info = e.player_info(slot);
    if info.name_bytes.is_empty() {
        return Err(NOT_FOUND);
    }
    let (frags, deaths, team) = crate::hud::with(|h| h.players.get(slot as usize).map_or((0, 0, 0), |p| (p.frags, p.deaths, p.team)));
    let lt = local_team();
    let mut flags = 0;
    if info.this_player {
        flags |= player::LOCAL;
    }
    if info.spectator {
        flags |= player::SPECTATOR;
    }
    if lt != 0 && team == lt {
        flags |= player::SAME_TEAM;
    }
    let mut row = PlayerScore { slot, team, frags, deaths, ping: info.ping, flags, ..Default::default() };
    fill(&mut row.name, &info.name_bytes);
    Ok(row)
}

/// `ClientExtApi::scores`: every connected player's row, by slot.
pub fn scores() -> Vec<PlayerScore> {
    (1..=engine().max_clients()).filter_map(|s| player_score(s).ok()).collect()
}

/// `ClientExtApi::aim`: the crosshair ID's trace (`ClEngine::crosshair_trace`) from the eye along
/// the view.
pub fn aim() -> Result<AimInfo, i32> {
    let e = engine();
    let index = local_index();
    if index <= 0 {
        return Err(NOT_FOUND);
    }
    let start = eye();
    let fwd = e.angle_vectors(e.view_angles()).0;
    let end = add(start, fwd.map(|v| v * AIM_RANGE));
    let (fraction, point, ent) = e.crosshair_trace(start, end, index).ok_or(UNAVAILABLE)?;
    let max = e.max_clients();
    let kind = match ent {
        _ if fraction >= 1.0 => hit::NONE,
        0 => hit::WORLD,
        n if (1..=max).contains(&n) => hit::PLAYER,
        n if n > 0 => hit::ENTITY,
        _ => hit::WORLD,
    };
    let slot = if kind == hit::PLAYER { ent } else { 0 };
    let lt = local_team();
    let same = slot > 0 && lt != 0 && crate::hud::with(|h| h.players.get(slot as usize).is_some_and(|p| p.team == lt));
    Ok(AimInfo {
        hit: kind,
        entity: if kind == hit::NONE { -1 } else { ent.max(0) },
        player: slot,
        same_team: same as i32,
        distance: fraction.clamp(0.0, 1.0) * AIM_RANGE,
        point,
    })
}

/// A world-only trace (`EV_PlayerTrace` with `PM_WORLD_ONLY`, point hull), or `None` without the
/// event API.
fn world_trace(start: [f32; 3], end: [f32; 3]) -> Option<TraceInfo> {
    engine().event_api()?;
    let mut tr = render::player_trace(start, end, 2, PM_WORLD_ONLY, -1);
    let hit = tr.fraction < 1.0 || tr.allsolid != 0;
    let entity = if hit { render::index_from_trace(&mut tr).max(0) } else { -1 };
    Some(TraceInfo { fraction: tr.fraction, end: tr.endpos, entity })
}

/// `ClientExtApi::trace_line`.
pub fn trace_line(start: [f32; 3], end: [f32; 3]) -> Result<TraceInfo, i32> {
    world_trace(start, end).ok_or(UNAVAILABLE)
}

/// The bearing (yaw from the view direction, -180..180, positive left) and elevation (positive
/// up) of `target` seen from `eye` with view `angles` (GoldSrc: yaw grows to the left, pitch
/// grows downwards).
pub fn bearing_elevation(eye: [f32; 3], angles: [f32; 3], target: [f32; 3]) -> (f32, f32) {
    let d = sub(target, eye);
    let yaw = d[1].atan2(d[0]).to_degrees();
    let pitch = d[2].atan2((d[0] * d[0] + d[1] * d[1]).sqrt()).to_degrees();
    let mut bearing = yaw - angles[1];
    while bearing > 180.0 {
        bearing -= 360.0;
    }
    while bearing < -180.0 {
        bearing += 360.0;
    }
    (bearing, pitch + angles[0])
}

/// The players in view (`ClientExtApi::visible_players`), nearest first: not the local player,
/// in this frame's packet, drawn, not an observer, not dead ([`super::alive`]), on the screen, and
/// reached by a world trace from the eye to its head or its centre. Cached per frame by [`super`].
pub fn visible_players() -> Vec<VisiblePlayer> {
    let e = engine();
    let Some(me) = e.local_player() else { return Vec::new() };
    let (my_index, my_msg) = (me.index, me.curstate.messagenum);
    if e.event_api().is_none() {
        return Vec::new();
    }
    let screen = crate::hud::screen();
    let (start, angles) = (eye(), e.view_angles());
    let lt = local_team();
    let mut out = Vec::new();
    for slot in 1..=e.max_clients() {
        if slot == my_index {
            continue;
        }
        let Some(p) = e.entity(slot) else { continue };
        let (msg, effects, origin, model, movetype) = (p.curstate.messagenum, p.curstate.effects, p.origin, p.model, p.curstate.movetype);
        let observer = crate::state::with(|s| s.spectator.get(slot as usize - 1).copied().unwrap_or(false));
        if model.is_null() || msg != my_msg || effects & EF_NODRAW as i32 != 0 || observer || super::player_dead(slot, msg, movetype) {
            continue;
        }
        let Some((x, y)) = e.world_to_screen(origin, &screen) else { continue };
        if !(0..screen.width).contains(&x) || !(0..screen.height).contains(&y) {
            continue;
        }
        let seen = [add(origin, [0.0, 0.0, HEAD_HEIGHT]), origin]
            .into_iter()
            .any(|target| world_trace(start, target).is_some_and(|t| t.fraction >= 1.0));
        if !seen {
            continue;
        }
        let (bearing, elevation) = bearing_elevation(start, angles, origin);
        let team = crate::hud::with(|h| h.players.get(slot as usize).map_or(0, |p| p.team));
        out.push(VisiblePlayer {
            slot,
            team,
            same_team: (lt != 0 && team == lt) as i32,
            distance: len(sub(origin, start)),
            bearing,
            elevation,
            screen: [x as f32, y as f32],
            origin,
        });
    }
    out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    out
}

/// `ClientExtApi::view`.
pub fn view() -> ViewInfo {
    let s = crate::hud::screen();
    let mut v = ViewInfo {
        width: s.width,
        height: s.height,
        fov: crate::state::with(|st| st.fov),
        intermission: crate::hud::intermission() as i32,
        ..Default::default()
    };
    fill(&mut v.map, super::events::map_name(&render::level_name()).as_bytes());
    v
}

/// `ClientExtApi::world_to_screen`: pixels, or `NOT_FOUND` behind the view.
pub fn world_to_screen(point: [f32; 3]) -> Result<[f32; 2], i32> {
    let e = engine();
    e.tri_api().ok_or(UNAVAILABLE)?;
    let (x, y) = e.world_to_screen(point, &crate::hud::screen()).ok_or(NOT_FOUND)?;
    Ok([x as f32, y as f32])
}

/// A TS weapon id's name, as the ruleset's tables have it ("Kung Fu" for 0, "" for none).
pub fn weapon_name(id: i32) -> String {
    if id == 0 {
        return "Kung Fu".into();
    }
    u8::try_from(id).ok().and_then(|id| shared::weapons::table::tables().weapon(id)).map(|w| w.name.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_flag_bits() {
        assert_eq!(local_flags(false, false, false, false), 0);
        assert_eq!(local_flags(true, true, false, false), local::ON_GROUND | local::DUCKING);
        assert_eq!(local_flags(false, false, true, true), local::OBSERVER | local::INTERMISSION);
    }

    #[test]
    fn bearings() {
        let eye = [0.0; 3];
        // Facing +x (yaw 0): a target to the left (+y) is +90, to the right -90, behind ±180.
        assert!((bearing_elevation(eye, [0.0, 0.0, 0.0], [100.0, 0.0, 0.0]).0).abs() < 1e-4);
        assert!((bearing_elevation(eye, [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]).0 - 90.0).abs() < 1e-4);
        assert!((bearing_elevation(eye, [0.0, 0.0, 0.0], [0.0, -100.0, 0.0]).0 + 90.0).abs() < 1e-4);
        assert!((bearing_elevation(eye, [0.0, 350.0, 0.0], [100.0, 0.0, 0.0]).0 - 10.0).abs() < 1e-3, "wraps");
        // Looking down 30 degrees (pitch +30) at a target level with the eye: it is 30 above.
        let (_, el) = bearing_elevation(eye, [30.0, 0.0, 0.0], [100.0, 0.0, 0.0]);
        assert!((el - 30.0).abs() < 1e-4);
        let (_, el) = bearing_elevation(eye, [0.0, 0.0, 0.0], [100.0, 0.0, 100.0]);
        assert!((el - 45.0).abs() < 1e-4);
    }
}
