//! Spectating: TS's `CHudSpectator` (HUD element 13, `gHUD+0x6d8`, vtable `1007cb00`), the
//! spectator view (`V_CalcSpectatorRefdef` `0x10068f70`) and the client's own
//! `PM_SpectatorMove` (`0x1002c790`). Reference: decomp client/architecture-hud.md §4 and
//! rendering-effects-prediction.md §7.
//!
//! TS uses the pre-HLTV-rewrite observer numbering (`shared::ts::obs`): 1 chase locked, 2 chase
//! free, 3 roaming, 4 the director's locked view (HLTV only), 5 in-eye.
//!
//! For players the server drives the mode and target (`iuser1`/`iuser2`). Client prediction runs
//! TS's client spectator move ([`movement`]), which places the camera and tells the view to use
//! it, keeps the last camera when the target leaves the PVS, and runs the director's locked view.
//! The view adds TS's in-eye view (the target's eyes and gun) and the `cl_vsmoothing` rings.
//! [`panel`] is TS's VGUI spectator bar. The HLTV-only parts of `CHudSpectator` are the director
//! events, `specmode`, the director-reset camera, the overview map and the name tags
//! ([`overview`]), and the inset view (unreachable in TS, which never sets an inset mode).

mod movement;
mod overview;
pub(crate) use overview::atof;
mod panel;
pub(crate) use panel::height as panel_height;

pub(crate) use movement::smooth;
pub use movement::{begin_player_move, in_eye_angles, spectator_move};
pub use overview::{add_entity as add_overview_entity, death as death_notice, draw as draw_overview};

use crate::hud::text::localise;
use crate::state;
use goldsrc::Global;
use goldsrc::client::engine;
use goldsrc::ext::render::{self as r, Vec3, vec3 as v};
use goldsrc::ext::spectator as spec;
use goldsrc::sys::*;
use shared::protocol::hud_wire::director;
use shared::ts::{obs, stunt};
use std::cell::RefCell;

/// `cl_vsmoothing` ring size (`ORIGIN_BACKUP`).
const RING: usize = 64;

/// One `cl_vsmoothing` ring (`0x10190f98` angles / `0x10190b98` origins, 64 entries with times).
#[derive(Clone)]
struct Ring {
    v: [Vec3; RING],
    time: [f32; RING],
    next: usize,
}

impl Default for Ring {
    fn default() -> Self {
        Self { v: [[0.0; 3]; RING], time: [0.0; RING], next: 0 }
    }
}

impl Ring {
    fn push(&mut self, x: Vec3, time: f32) {
        self.v[self.next & (RING - 1)] = x;
        self.time[self.next & (RING - 1)] = time;
        self.next = self.next.wrapping_add(1);
    }

    /// The slots of the samples around `t` (SDK `V_CalcNormalRefdef` ring search), or `None`
    /// when the history doesn't reach back that far.
    fn pair(&self, t: f32) -> Option<(usize, usize)> {
        let mut i = 1;
        let mut found = self.next.wrapping_sub(2);
        while i < RING - 1 {
            if self.time[found & (RING - 1)] <= t {
                break;
            }
            i += 1;
            found = found.wrapping_sub(1);
        }
        let (a, b) = (found & (RING - 1), found.wrapping_add(1) & (RING - 1));
        if i >= RING - 1 || self.time[a] == 0.0 {
            return None;
        }
        Some((a, b))
    }

    /// The pair of samples around `t` and the fraction between them.
    fn sample(&self, t: f32) -> Option<(Vec3, Vec3, f32)> {
        let (a, b) = self.pair(t)?;
        let dt = self.time[b] - self.time[a];
        (dt > 0.0).then(|| (self.v[a], self.v[b], ((t - self.time[a]) / dt).min(1.0)))
    }

    /// The angle ring's fraction ([`angle_ring_frac`]).
    fn angle_frac(&self, a: usize, b: usize, t: f32) -> Option<f32> {
        angle_ring_frac(self.time[a], self.time[b], t)
    }
}

/// The fraction both angle rings hand `InterpolateAngles` (this view's `0x10069291..0x100692e4`,
/// the normal view's `0x10067c09..0x10067c48`): `t − ta` over `tb − ta`, unrounded until the
/// `float` argument, `None` unless the span is > 0 (`test ah, 0x41`), and capped at 1 (`fcomp`
/// with C0, so a NaN becomes 1 too).
pub(crate) fn angle_ring_frac(ta: f32, tb: f32, t: f32) -> Option<f32> {
    let (ta, tb) = (ta as f64, tb as f64);
    let dt = tb - ta;
    if dt <= 0.0 || dt.is_nan() {
        return None;
    }
    let frac = (t as f64 - ta) / dt;
    Some(if 1.0 < frac || frac.is_nan() { 1.0 } else { frac as f32 })
}

#[derive(Default)]
struct Spectator {
    /// Inset mode (`+0x14`; only ever 0 in TS).
    pip: i32,
    /// Target picked for the locked view (`+0x948`).
    locked_target: i32,
    /// Last director camera: primary and secondary entity (`+0xca0`/`+0xca4`) and flags
    /// (`0x10192158`, read by the client `PM_SpectatorMove` for the locked view).
    last_primary: i32,
    last_secondary: i32,
    director_flags: i32,
    /// Map camera from the last director reset (`0x10191d40`); pending (`0x10192154`) until
    /// player movement has moved a roaming spectator there once.
    default_origin: Vec3,
    pin_pending: bool,
    /// The angles the pin puts the camera at: zero after a director reset (TS 3.0's
    /// `0x10191d2c`, never written); a ruleset's HLTV camera jump sets them
    /// ([`HltvHost::jump_camera`]).
    pin_angles: Vec3,
    /// `sprites/crosshairs.spr` (`+0xc8c`).
    crosshair: HSPRITE,
    // ---- client `PM_SpectatorMove` → view ----
    /// `cl_vsmoothing` flags (`0x101914ec`, [`movement::smooth`]).
    smooth_flags: i32,
    /// The view uses the camera player movement computed (`0x1019149c` angles, `0x101914f0`
    /// origin) ...
    angles_override: bool,
    origin_override: bool,
    /// ... kept here (`0x10190b6c`, `0x101913e4`); the view writes its own camera back while the
    /// overrides are off.
    saved_angles: Vec3,
    saved_origin: Vec3,
    /// The locked view's eased angles (`0x100e0eec`).
    locked_angles: Vec3,
    // ---- view (`V_CalcSpectatorRefdef` statics) ----
    /// Angle ring and the last angles stored in it (`0x10190b44`).
    angles: Ring,
    last_angles: Vec3,
    /// Origin ring and the last (smoothed) view origin (`0x10190b60`).
    origins: Ring,
    last_origin: Vec3,
    /// Cached in-eye gun: the target's `weaponmodel` and the TS weapon it belongs to
    /// (`0x10191db0`/`0x10097218`).
    gun_weaponmodel: i32,
    gun: Option<u8>,
    // ---- HUD ----
    overview: overview::Overview,
    panel: panel::Panel,
}

static SPEC: Global<RefCell<Option<Spectator>>> = Global(RefCell::new(None));

fn with<R>(f: impl FnOnce(&mut Spectator) -> R) -> R {
    let mut g = SPEC.0.borrow_mut();
    f(g.get_or_insert_with(Spectator::default))
}

/// `CHudSpectator::Init` (`0x10022c70`): the `specmode` command (`cl_drawnames` is registered
/// with the HUD cvars).
pub fn init() {
    with(|s| *s = Spectator::default());
    crate::gameplay::current().hltv_reset(true);
    engine().add_command(c"specmode", cmd_specmode);
}

/// `CHudSpectator::VidInit` (`0x10022fe0`): the overview sprites and the crosshair. It also
/// zeroes the observer input timer (`+0xc94`), which the port keeps in `input`.
pub fn vid_init() {
    let sprites = overview::load_sprites();
    let h = engine().spr_load("sprites/crosshairs.spr");
    with(|s| {
        s.overview.sprites = sprites;
        s.crosshair = h;
    });
    crate::input::reset_key_tap();
}

/// `specmode <n>` (`0x10022c10`): HLTV spectators pick a camera mode (and turn the inset off).
extern "C" fn cmd_specmode() {
    let e = engine();
    if !spec::is_spectate_only() {
        e.print("specmode only in HLTV mode available.\n");
        return;
    }
    if e.cmd_argc() < 2 {
        e.print("usage:  specmode < modenumber >\n");
        return;
    }
    let mode = e.cmd_argv(1).to_str().ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    // TS passes inset mode 0 (the push before the `Cmd_Argv` call stays on the stack).
    set_modes(mode, 0);
}

/// What `set_modes` decided, applied by the caller (pure, so it can be tested).
#[derive(Debug, Default, PartialEq)]
struct ModeChange {
    /// Centre texts to show (`%c#Spec_Mode_Inset%d`, `%c#Spec_Mode%d` through `TextMsg`).
    texts: Vec<String>,
    /// Crosshair: `Some(true)` = the spectator crosshair, `Some(false)` = none.
    crosshair: Option<bool>,
}

/// `CHudSpectator::SetModes` (`0x100234d0`). Requests use the `specmode` numbers: 2 selects the
/// director's locked view (4), 3 roaming and 5 in-eye; 1 and 4 change nothing but the text.
fn set_modes_in(s: &mut Spectator, st: &mut state::Shared, mode: i32, pip: i32, valid_target: impl Fn(i32) -> bool) -> Option<ModeChange> {
    if !(1..=5).contains(&mode) {
        return None;
    }
    let mut out = ModeChange::default();
    if pip != s.pip {
        s.pip = pip;
        out.texts.push(format!("#Spec_Mode_Inset{pip}"));
    }
    if mode == st.spec_mode {
        return Some(out);
    }
    match mode {
        2 => st.spec_mode = obs::LOCKED_VIEW,
        3 => {
            st.spec_mode = obs::ROAMING;
            s.locked_target = 0;
            st.spec_target = 0;
            st.spec_flags = 0;
        }
        5 => st.spec_mode = obs::IN_EYE,
        _ => {}
    }
    out.texts.push(format!("#Spec_Mode{mode}"));
    if st.spec_mode == obs::LOCKED_VIEW && s.locked_target == 0 && valid_target(s.last_primary) {
        st.spec_target = s.last_primary;
        s.locked_target = s.last_primary;
        st.spec_flags = s.last_secondary;
    }
    out.crosshair = Some(matches!(st.spec_mode, obs::ROAMING | obs::IN_EYE));
    Some(out)
}

/// `IsActivePlayer` (`0x100236d0`; TS 2.1's `0x1004b9e0` is the same): the director's target
/// for the locked view, and the 2.1 HLTV controls' ([`HltvHost::active_player`]).
fn valid_player(idx: i32) -> bool {
    let e = engine();
    let local = e.local_player().map_or(0, |p| p.index);
    is_active_player(e.entity(idx), local, || !e.player_info(idx).name_bytes.is_empty())
}

/// The tests of `IsActivePlayer`: an entity that is a player (`+0x4`), solid (`curstate.solid`,
/// `+0x2ea`; a dead player or a spectator is `SOLID_NOT`), not the local player, and whose slot
/// has a player-info name (`[0x1018fbd8 + index × 20]`). TS doesn't look at the model.
fn is_active_player(ent: Option<&cl_entity_t>, local: i32, named: impl FnOnce() -> bool) -> bool {
    ent.is_some_and(|e| e.player != 0 && e.curstate.solid != 0 && e.index != local) && named()
}

/// `SetModes`, from `specmode` and the director reset. A ruleset may have a `SetModes` of its own
/// ([`ClientRules::hltv_set_modes`](crate::gameplay::ClientRules::hltv_set_modes)).
pub fn set_modes(mode: i32, pip: i32) {
    if crate::gameplay::current().hltv_set_modes(&mut EngineHltv, mode, pip) {
        return;
    }
    let change = with(|s| state::with(|st| set_modes_in(s, st, mode, pip, valid_player)));
    let Some(change) = change else {
        engine().print("Invalid spectator mode.\n");
        return;
    };
    for t in &change.texts {
        engine().center_print(&localise(t));
    }
    if let Some(on) = change.crosshair {
        let h = with(|s| s.crosshair);
        if on && h != 0 {
            spec::set_crosshair(h, [24, 48, 0, 24], [255, 255, 255]);
        } else {
            spec::set_crosshair(0, [0; 4], [0; 3]);
        }
    }
    // `SetModes` ends with `UpdateSpectatorPanel` whenever the mode number was valid.
    panel::update();
}

/// `HUD_DirectorEvent` (`0x1001ab60`) → `CHudSpectator::DirectorMessage` (`0x10023380`): TS's
/// pre-SDK-2.3 director interface. `DRC_ACTIVE` resets spectating (locked view, no inset, camera
/// on the map's first `info_player_start`); `DRC_CAMERA` records the director's targets and, in
/// the locked view, follows them.
pub fn director_event(command: u8, primary: i32, secondary: i32, flags: i32) {
    match command {
        director::DRC_ACTIVE => {
            with(|s| {
                s.pip = 0;
                s.locked_target = 0;
                s.last_primary = 0;
                s.last_secondary = 0;
                s.pin_pending = false;
                s.overview.reset_entities();
            });
            crate::input::reset_key_tap();
            crate::gameplay::current().hltv_reset(false);
            // `0x10023380` also zeroes `g_iPlayerClass` and `g_iTeamNumber` (2.1 `0x1004af10` too).
            state::with(|st| {
                st.spec_mode = obs::NONE;
                st.player_class = 0;
                st.team_number = 0;
            });
            set_modes(2, 0);
            with(|s| s.overview.reset());
            // `0x10022da0`: the map camera, which the view also uses until player movement
            // takes over.
            let origin = map_camera();
            with(|s| {
                s.default_origin = origin;
                s.pin_angles = [0.0; 3];
                s.pin_pending = true;
                s.saved_origin = origin;
                s.origin_override = true;
            });
        }
        director::DRC_CAMERA => {
            let locked = state::with(|st| {
                let locked = st.spec_mode == obs::LOCKED_VIEW;
                if locked {
                    st.spec_target = primary;
                    st.spec_flags = secondary;
                }
                locked
            });
            with(|s| {
                if locked {
                    s.locked_target = primary;
                }
                s.last_primary = primary;
                s.last_secondary = secondary;
                s.director_flags = flags;
            });
        }
        _ => {
            if engine().cvar_float(c"developer") != 0.0 {
                engine().print("CHudSpectator::DirectorEvent: unknown director command.\n");
            }
        }
    }
}

/// `HUD_DirectorMessage` (port addition): the engine export SDK 2.3+ engines call with a
/// `svc_director` payload. TS only exports the older `HUD_DirectorEvent`, which these engines
/// never call. The two commands TS handles map onto the new numbering: `DRC_CMD_START` (1) is
/// TS's `DRC_ACTIVE`, and `DRC_CMD_EVENT` (2) carries the same `short, short, long` targets as
/// TS's `DRC_CAMERA` (2).
pub fn director_message(buf: &[u8]) {
    if let Some((cmd, primary, secondary, flags)) = parse_director_message(buf) {
        director_event(cmd, primary, secondary, flags);
    }
}

/// `svc_director` payload → TS's `(command, primary, secondary, flags)`.
fn parse_director_message(buf: &[u8]) -> Option<(u8, i32, i32, i32)> {
    let rd16 = |o: usize| buf.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as i32);
    let rd32 = |o: usize| buf.get(o..o + 4).map_or(0, |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    match buf.first() {
        Some(1) => Some((director::DRC_ACTIVE, 0, 0, 0)),
        Some(2) => Some((director::DRC_CAMERA, rd16(1), rd16(3), rd32(5))),
        _ => None,
    }
}

/// `0x10022da0`: the camera position after a director reset. TS walks the BSP entity lump and
/// keeps the last `origin` read before the first `info_player_start` classname.
fn map_camera() -> Vec3 {
    let bsp = r::load_file(&r::level_name()).unwrap_or_default();
    let ents = crate::effects::emitters::entity_lump(&bsp).map(crate::effects::emitters::parse_entities).unwrap_or_default();
    camera_from_entities(&ents)
}

fn camera_from_entities(ents: &[Vec<(String, String)>]) -> Vec3 {
    let mut origin = [0.0; 3];
    for e in ents {
        for (k, val) in e {
            if k == "origin" {
                origin = parse_vec(val);
            }
            if k == "classname" && val == "info_player_start" {
                return origin;
            }
        }
    }
    origin
}

/// `0x10022d00`: up to three numbers separated by spaces; missing ones are 0.
fn parse_vec(s: &str) -> Vec3 {
    let mut out = [0.0; 3];
    for (o, t) in out.iter_mut().zip(s.split(' ')) {
        *o = t.trim().parse().unwrap_or(0.0);
    }
    out
}

/// Spectating someone in-eye (`g_iUser1 == 5`).
pub fn in_eye() -> bool {
    state::with(|s| s.spec_mode == obs::IN_EYE)
}

/// The inset mode (`CHudSpectator+0x14`, `0x100d000c`): 0 in TS 3.0, which has no inset; a
/// ruleset's HLTV controls may set it ([`HltvHost::set_inset`]).
pub fn pip() -> i32 {
    with(|s| s.pip)
}

/// What the client `PM_SpectatorMove` (or the director reset) left for `V_CalcNormalRefdef`
/// (`0x10067890`): the `cl_vsmoothing` flags ([`smooth`]) and the camera origin and angles it
/// says to use (`0x101914f0`/`0x101913e4`, `0x1019149c`/`0x10190b6c`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ViewOverrides {
    pub smooth_flags: i32,
    pub origin: Option<Vec3>,
    pub angles: Option<Vec3>,
}

pub fn view_overrides() -> ViewOverrides {
    with(|s| ViewOverrides {
        smooth_flags: s.smooth_flags,
        origin: s.origin_override.then_some(s.saved_origin),
        angles: s.angles_override.then_some(s.saved_angles),
    })
}

/// The normal view's smoothed origin becomes the saved camera origin (`0x10068974`).
pub fn store_smoothed_origin(origin: Vec3) {
    with(|s| s.saved_origin = origin);
}

/// The normal view's smoothed angles become the saved camera angles (`0x10067c73`).
pub fn store_smoothed_angles(angles: Vec3) {
    with(|s| s.saved_angles = angles);
}

// ---- view ---------------------------------------------------------------------------------------

/// `V_CalcSpectatorRefdef` (`0x10068f70`). The camera starts at `simorg` with the mouse angles,
/// or at the camera the client's `PM_SpectatorMove` computed when it said so ([`movement`]):
/// that puts a chasing spectator behind its target looking its way (chase locked), on the
/// director's locked view, or keeps the last camera when the target left the PVS. TS's
/// chase-position code here only writes globals the function overwrites at its end. On the first
/// pass the `cl_vsmoothing` rings run and in-eye moves to the target's eyes; with an inset mode
/// (never set in TS) a second pass draws the inset window. In-eye shows the target's gun.
pub fn calc_refdef(p: &mut ref_params_t) {
    let (mode, target) = state::with(|s| (s.spec_mode, s.spec_target));
    let first = p.nextView == 0;
    p.vieworg = p.simorg;
    p.viewangles = p.cl_viewangles;
    with(|s| {
        if s.origin_override {
            p.vieworg = s.saved_origin;
        } else {
            s.saved_origin = p.vieworg;
        }
        if s.angles_override {
            p.viewangles = s.saved_angles;
        } else {
            s.saved_angles = p.viewangles;
        }
    });
    if first {
        smooth(p);
    }
    with(|s| s.last_origin = p.vieworg);
    // A ruleset's own mode passes (TS 2.1's HLTV main and inset modes).
    if crate::gameplay::current().hltv_calc_refdef(&mut EngineHltv, p) {
        return;
    }
    if first {
        match mode {
            obs::IN_EYE => in_eye_position(target, &mut p.vieworg, &mut p.viewangles),
            obs::CHASE_LOCKED | obs::CHASE_FREE | obs::ROAMING => {}
            _ => p.onlyClientDraw = 0,
        }
        with(|s| {
            if s.pip != 0 {
                p.nextView = 1;
            }
            s.overview.draw_cycle = 0;
        });
    } else {
        inset_view(p);
    }
    let weaponmodel = (target != 0).then(|| engine().entity(target)).flatten().map(|t| t.curstate.weaponmodel);
    match weaponmodel {
        Some(wm) if mode == obs::IN_EYE => in_eye_gun(p, target, wm),
        _ => {
            if let Some(vm) = r::view_model() {
                vm.model = std::ptr::null_mut();
            }
            with(|s| {
                s.gun_weaponmodel = 0;
                s.gun = None;
            });
        }
    }
}

/// The inset pass of `V_CalcSpectatorRefdef` (`0x1006970b`): the overview file's inset window
/// (640×480 units, `__ftol`), in-eye of the locked target (mode 2) or the overview map alone
/// (mode 3). TS never sets an inset mode, so this never runs there either.
fn inset_view(p: &mut ref_params_t) {
    let sc = engine().screen();
    let (sx, sy) = (sc.width as f32 * (1.0 / 640.0), sc.height as f32 * (1.0 / 480.0));
    let (pip, [x, y, w, h], locked) = with(|s| (s.pip, s.overview.data.inset, s.locked_target));
    p.viewport = [(sx * x as f32) as i32, (sy * y as f32) as i32, (sx * w as f32) as i32, (sy * h as f32) as i32];
    p.nextView = 0;
    p.onlyClientDraw = 0;
    match pip {
        2 => in_eye_position(locked, &mut p.vieworg, &mut p.viewangles),
        3 => {
            p.onlyClientDraw = 1;
            let (org, ang) = with(|s| overview::map_view(s, p.cl_viewangles));
            p.vieworg = org;
            p.viewangles = ang;
        }
        _ => {}
    }
    with(|s| s.overview.draw_cycle = 1);
}

/// `cl_vsmoothing` (the angle ring `0x10190f98` and origin ring `0x10190b98`), smoothing what
/// the client's `PM_SpectatorMove` flagged (angles in in-eye and the locked view, origins also in
/// chase locked). A smoothed camera is also what the view keeps for the next frame.
fn smooth(p: &mut ref_params_t) {
    let amount = crate::input::cvars().cl_vsmoothing.value();
    let time = p.time;
    // `cl_vsmoothing->value` is tested with `fcomp`/`test ah, 0x40`: a NaN counts as 0.
    let smoothing = !x87_eq0(amount as f64);
    with(|s| {
        // `0x10069122`: the unrounded `Length` of the change, a NaN counting as none.
        if !x87_eq0(length(v::sub(p.viewangles, s.last_angles))) {
            s.angles.push(p.viewangles, time);
            s.last_angles = p.viewangles;
        }
        let t = time - amount;
        if smoothing
            && s.smooth_flags & movement::smooth::ANGLES != 0
            && let Some((a, b)) = s.angles.pair(t)
        {
            // The engine's `AngleVectors` (`0x10069255`), as TS calls it.
            let e = engine();
            let between = angle_between(e.angle_vectors(s.angles.v[a]).0, e.angle_vectors(s.angles.v[b]).0);
            // `< 22.5` with C0 (`0x100692bb`): a NaN angle passes.
            if let Some(frac) = s.angles.angle_frac(a, b, t)
                && (between < 22.5 || between.is_nan())
            {
                // `InterpolateAngles` normalises the two ring entries in place.
                let (mut x, mut y) = (s.angles.v[a], s.angles.v[b]);
                s.saved_angles = interpolate_angles(&mut x, &mut y, frac);
                (s.angles.v[a], s.angles.v[b]) = (x, y);
                p.viewangles = s.saved_angles;
            }
        }
        // TS compares against the last *smoothed* origin.
        if v::len(v::sub(p.vieworg, s.last_origin)) != 0.0 {
            s.origins.push(p.vieworg, time);
        }
        if amount != 0.0
            && s.smooth_flags & movement::smooth::ORIGIN != 0
            && let Some((a, b, frac)) = s.origins.sample(time - amount)
            && v::len(v::sub(b, a)) < 64.0
        {
            let org = v::ma(a, frac, v::sub(b, a));
            p.vieworg = v::add(p.vieworg, v::sub(org, p.simorg));
            s.saved_origin = p.vieworg;
        }
    });
}

/// HUD and VGUI drawing, after the HUD elements: `CHudSpectator::Draw`'s HLTV name tags (a HUD
/// element: not in intermission or with `HIDEHUD_ALL`), then the spectator panel, which the
/// viewport refreshes first.
pub fn redraw() {
    panel::think();
    // Port addition: a client extension may hide it (issue #314).
    crate::hud::elements::draw(ts_client_ext_api::hud::SPECTATOR, panel::draw);
}

/// `CHudSpectator::Draw` (`0x10023080`, the HLTV name tags), called from the HUD's element pass
/// in TS's element order. A ruleset's own `Draw` may replace the name tags.
pub fn draw_hltv_names() {
    if !crate::gameplay::current().hltv_draw(&mut EngineHltv) {
        overview::draw_names();
    }
}

/// What a ruleset's own HLTV spectator controls read and drive
/// ([`ClientRules::hltv_buttons`](crate::gameplay::ClientRules::hltv_buttons) and the other
/// `hltv_*` hooks). TS 3.0 has none: its `+` commands only run the `respawn` key tap. The engine
/// implementation is [`EngineHltv`]; tests pass their own.
pub trait HltvHost {
    /// `GetClientTime`, widened as `fstp qword` does.
    fn time(&self) -> f64;
    /// `IsSpectateOnly`: an HLTV viewer.
    fn spectate_only(&self) -> bool;
    /// The observer mode (`g_iUser1`).
    fn observer_mode(&self) -> i32;
    fn set_observer_mode(&mut self, mode: i32);
    /// The observer target and flags (`g_iUser2`, `g_iUser3`).
    fn set_observer_target(&mut self, target: i32, flags: i32);
    /// The spectator's own target (TS 3.0 `+0x948`).
    fn locked_target(&self) -> i32;
    fn set_locked_target(&mut self, target: i32);
    /// The last director camera's primary and secondary entity (3.0 `+0xca0`, `+0xca4`).
    fn last_director(&self) -> (i32, i32);
    /// The inset mode the view draws (3.0 `+0x14`).
    fn set_inset(&mut self, mode: i32);
    /// `IsActivePlayer` (3.0 `0x100236d0`): a player entity other than the local one, solid
    /// (`curstate.solid`, `+0x2ea`), with a name in the player info.
    fn active_player(&self, index: i32) -> bool;
    fn player_name(&self, index: i32) -> String;
    /// The player-info name's raw bytes (`g_PlayerInfoList[index].name`, empty without one).
    fn player_name_bytes(&self, index: i32) -> Vec<u8>;
    /// The scoreboard team (`g_PlayerExtraInfo[index].teamnumber`).
    fn player_team(&self, index: i32) -> i32;
    /// An entity's origin and angles (`+0xb48`, `+0xb54`).
    fn entity_camera(&self, index: i32) -> Option<(Vec3, Vec3)>;
    /// What `V_GetInEyePos` reads of its target, when it passes the function's tests: an entity
    /// that is a player (`+0x4`) with a player-info name. `None` leaves the camera alone.
    fn in_eye_target(&self, target: i32) -> Option<InEyeTarget>;
    /// The last refdef's view origin and angles (`v_origin`, `v_angles`).
    fn view_camera(&self) -> (Vec3, Vec3);
    /// `SetViewAngles(angles)` and the jump flag: the next spectator move puts a roaming camera
    /// at `origin` with `angles` (`vJumpOrigin`, `vJumpAngles`, `iJumpSpectator`).
    fn jump_camera(&mut self, origin: Vec3, angles: Vec3);
    fn print(&mut self, text: &str);
    /// A `TextMsg` centre print of a `titles.txt` key.
    fn center_text(&mut self, key: &str);
    /// The spectator crosshair (`sprites/crosshairs.spr` 24..48 × 0..24, white) or none.
    fn set_crosshair(&mut self, on: bool);
    /// `UpdateSpectatorPanel`.
    fn update_panel(&mut self);
    /// The overview file's zoom and origin (`m_OverviewData`).
    fn overview(&self) -> (f32, Vec3);
    /// The map view's zoom, origin and angles.
    fn map_view(&self) -> (f32, Vec3, Vec3);
    fn set_map_view(&mut self, zoom: f32, origin: Vec3);
    /// The map view's angles, which the view's map camera sets and the overview draws with.
    fn set_map_angles(&mut self, angles: Vec3);
    /// The overview file's inset window in 640×480 units: x, y, width, height.
    fn overview_inset(&self) -> [i32; 4];
    /// The HUD's screen size (`ScreenWidth`, `ScreenHeight`).
    fn screen_size(&self) -> (i32, i32);
    /// Which view pass the overview draws for: the main view (false) or the inset (true).
    fn set_inset_pass(&mut self, inset: bool);
    /// Hides the view model (`model = NULL`).
    fn clear_view_model(&mut self);
    /// The name tags' screen positions per player slot (index + 1): x, y, and < 0 for none.
    fn name_tags(&self) -> Vec<[f32; 3]>;
    /// `cl_drawnames`.
    fn draw_names(&self) -> f32;
    /// `pfnDrawSetTextColor` and `pfnDrawConsoleString`.
    fn draw_text(&mut self, x: i32, y: i32, color: [f32; 3], text: &[u8]);
}

/// The fields of a `V_GetInEyePos` target ([`HltvHost::in_eye_target`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InEyeTarget {
    /// `origin` and `angles` (`+0xb48`, `+0xb54`).
    pub origin: Vec3,
    pub angles: Vec3,
    /// `curstate.solid` (`+0x2ea`) and `curstate.usehull` (`+0x378`).
    pub solid: i16,
    pub usehull: i32,
}

/// The [`InEyeTarget`] of `e` if it passes `V_GetInEyePos`'s tests (both TS builds): a player
/// entity whose slot has a player-info name.
fn in_eye_target_of(e: Option<&cl_entity_t>, named: impl FnOnce() -> bool) -> Option<InEyeTarget> {
    let e = e.filter(|e| e.player != 0).filter(|_| named())?;
    Some(InEyeTarget { origin: e.origin, angles: e.angles, solid: e.curstate.solid, usehull: e.curstate.usehull })
}

/// [`HltvHost`] on the engine and this module's state.
pub struct EngineHltv;

impl HltvHost for EngineHltv {
    fn time(&self) -> f64 {
        engine().client_time() as f64
    }
    fn spectate_only(&self) -> bool {
        spec::is_spectate_only()
    }
    fn observer_mode(&self) -> i32 {
        state::with(|st| st.spec_mode)
    }
    fn set_observer_mode(&mut self, mode: i32) {
        state::with(|st| st.spec_mode = mode);
    }
    fn set_observer_target(&mut self, target: i32, flags: i32) {
        state::with(|st| {
            st.spec_target = target;
            st.spec_flags = flags;
        });
    }
    fn locked_target(&self) -> i32 {
        with(|s| s.locked_target)
    }
    fn set_locked_target(&mut self, target: i32) {
        with(|s| s.locked_target = target);
    }
    fn last_director(&self) -> (i32, i32) {
        with(|s| (s.last_primary, s.last_secondary))
    }
    fn set_inset(&mut self, mode: i32) {
        with(|s| s.pip = mode);
    }
    fn active_player(&self, index: i32) -> bool {
        valid_player(index)
    }
    fn player_name(&self, index: i32) -> String {
        // One char per byte, like the HUD's network strings: no byte of the name is lost.
        crate::hud::saytext::string(&engine().player_info(index).name_bytes)
    }
    fn player_name_bytes(&self, index: i32) -> Vec<u8> {
        engine().player_info(index).name_bytes
    }
    fn player_team(&self, index: i32) -> i32 {
        crate::hud::with(|h| h.players.get(index as usize).map_or(0, |p| p.team))
    }
    fn entity_camera(&self, index: i32) -> Option<(Vec3, Vec3)> {
        engine().entity(index).map(|e| (e.origin, e.angles))
    }
    fn in_eye_target(&self, target: i32) -> Option<InEyeTarget> {
        let e = engine();
        in_eye_target_of(e.entity(target), || !e.player_info(target).name_bytes.is_empty())
    }
    fn view_camera(&self) -> (Vec3, Vec3) {
        (crate::view::origin(), crate::view::v_angles())
    }
    fn jump_camera(&mut self, origin: Vec3, angles: Vec3) {
        engine().set_view_angles(angles);
        with(|s| {
            s.default_origin = origin;
            s.pin_angles = angles;
            s.pin_pending = true;
        });
    }
    fn print(&mut self, text: &str) {
        engine().print(text);
    }
    fn center_text(&mut self, key: &str) {
        engine().center_print(&localise(key));
    }
    fn set_crosshair(&mut self, on: bool) {
        let h = with(|s| s.crosshair);
        if on {
            spec::set_crosshair(h, [24, 48, 0, 24], [255, 255, 255]);
        } else {
            spec::set_crosshair(0, [0; 4], [0; 3]);
        }
    }
    fn update_panel(&mut self) {
        panel::update();
    }
    fn overview(&self) -> (f32, Vec3) {
        with(|s| s.overview.overview())
    }
    fn map_view(&self) -> (f32, Vec3, Vec3) {
        with(|s| s.overview.map_view())
    }
    fn set_map_view(&mut self, zoom: f32, origin: Vec3) {
        with(|s| s.overview.set_map_view(zoom, origin));
    }
    fn set_map_angles(&mut self, angles: Vec3) {
        with(|s| s.overview.map_angles = angles);
    }
    fn overview_inset(&self) -> [i32; 4] {
        with(|s| s.overview.data.inset)
    }
    fn screen_size(&self) -> (i32, i32) {
        let sc = crate::hud::screen();
        (sc.width, sc.height)
    }
    fn set_inset_pass(&mut self, inset: bool) {
        with(|s| s.overview.draw_cycle = inset as i32);
    }
    fn clear_view_model(&mut self) {
        if let Some(vm) = r::view_model() {
            vm.model = std::ptr::null_mut();
        }
    }
    fn name_tags(&self) -> Vec<[f32; 3]> {
        with(|s| s.overview.tags().to_vec())
    }
    fn draw_names(&self) -> f32 {
        crate::hud::cvars().drawnames.value()
    }
    fn draw_text(&mut self, x: i32, y: i32, color: [f32; 3], text: &[u8]) {
        engine().draw_console_bytes(x, y, Some(color), text);
    }
}

/// An x87 `fcomp` against 0 tested with `test ah, 0x40` (C3): equal, or a NaN.
pub(crate) fn x87_eq0(x: f64) -> bool {
    x == 0.0 || x.is_nan()
}

/// client.dll's `Length` (`0x10058390`): `((0 + x²) + y²) + z²` and `fsqrt` at 53 bits, returned
/// unrounded.
pub(crate) fn length(a: Vec3) -> f64 {
    let (x, y, z) = (a[0] as f64, a[1] as f64, a[2] as f64);
    (((0.0 + x * x) + y * y) + z * z).sqrt()
}

/// `0x10029d70`: the angle between two vectors in degrees, 0 if either length is 0 (or NaN). TS
/// divides *after* the arc cosine, `acos(a·b) / (|a||b|) · 180 · (1/π)`, with no clamp: a dot
/// product past ±1 gives `_CIacos`'s NaN (`0x1006c74f`). The lengths are stored as `float`s, the
/// dot product (z first) and the rest stay `double`s until the caller's store. `1/π` is the
/// `double` at `0x1007d530`, bit for bit `FRAC_1_PI`.
fn angle_between(a: Vec3, b: Vec3) -> f32 {
    let (la, lb) = (length(a) as f32, length(b) as f32);
    if x87_eq0(la as f64) || x87_eq0(lb as f64) {
        return 0.0;
    }
    let d = |i: usize| a[i] as f64 * b[i] as f64;
    let dot = (d(2) + d(1)) + d(0);
    (dot.acos() / (la as f64 * lb as f64) * 180.0 * std::f64::consts::FRAC_1_PI) as f32
}

/// client.dll's `NormalizeAngles` (`0x10029cb0`) on a vector, in place.
fn normalize_angles(a: &mut Vec3) {
    for x in a.iter_mut() {
        *x = v::norm_angle(*x);
    }
}

/// `InterpolateAngles` (`0x10029cf0`): per-axis shortest-way blend. Like TS it normalises
/// `start` and `end` in place first. The difference stays a `double` (53-bit x87) through
/// `± 360`, `× frac` and `+ start` until the `float` store; `> 180` subtracts 360 and the
/// `< −180` test uses C0, so a NaN takes the `+ 360` branch. The result is normalised again.
pub(crate) fn interpolate_angles(start: &mut Vec3, end: &mut Vec3, frac: f32) -> Vec3 {
    normalize_angles(start);
    normalize_angles(end);
    let mut out = [0.0; 3];
    for i in 0..3 {
        let mut d = end[i] as f64 - start[i] as f64;
        if d > 180.0 {
            d -= 360.0;
        } else if d < -180.0 || d.is_nan() {
            d += 360.0;
        }
        out[i] = (d * frac as f64 + start[i] as f64) as f32;
    }
    normalize_angles(&mut out);
    out
}

/// `V_GetInEyePos` (`0x10068e20`): the target's origin raised to eye height (16 ducked, 28
/// standing, none while `NO_VIEW` stunts are on) and its angles with the model pitch undone. TS
/// scales the pitch by -π where the SDK uses -3. A dead target (not solid) rolls 80° and drops 8.
/// Like TS (`0x10068e42`), nothing is written unless the slot also has a player-info name
/// (`g_PlayerInfoList[idx].name`), so a stale `player` entity around a disconnect keeps the
/// previous values (issue #153).
pub(crate) fn in_eye_position(target: i32, org: &mut Vec3, ang: &mut Vec3) {
    let e = engine().entity(target).filter(|e| e.player != 0);
    let named = || !engine().player_info(target).name_bytes.is_empty();
    in_eye_from(e.filter(|_| named()), org, ang);
}

/// The writing part of [`in_eye_position`], for a target that passed TS's checks (`None`: leave
/// `org` and `ang` alone).
fn in_eye_from(e: Option<&cl_entity_t>, org: &mut Vec3, ang: &mut Vec3) {
    let Some(e) = e else { return };
    *org = e.origin;
    *ang = e.angles;
    ang[0] *= -std::f32::consts::PI;
    if e.curstate.solid == SOLID_NOT as i16 {
        ang[2] = 80.0;
        org[2] -= 8.0;
    } else if e.curstate.iuser4 & stunt::NO_VIEW == 0 {
        org[2] += if e.curstate.usehull == 1 { 16.0 } else { 28.0 };
    }
}

/// TS weapon id whose third-person model is `weaponmodel` (`0x10068ef0` compares the model name
/// with each weapon's p_ and silenced p_ model).
fn weapon_for_model(weaponmodel: i32) -> Option<u8> {
    if weaponmodel == 0 {
        return None;
    }
    (0..shared::weapons::MAX_WEAPONS as u8).find(|&id| {
        let (_, p, sil, _) = shared::weapons::models(id);
        [p, sil].into_iter().flatten().any(|m| r::model_index(&m) == weaponmodel)
    })
}

/// The in-eye half of `V_CalcSpectatorRefdef`: the view model shows the target's gun (with its
/// attachments' body groups), placed at the in-eye view plus the gun offset `V_CalcBob` last left
/// in the normal view, as in TS.
fn in_eye_gun(p: &ref_params_t, target: i32, weaponmodel: i32) {
    let Some(vm) = r::view_model() else { return };
    let gun = with(|s| {
        if s.gun_weaponmodel != weaponmodel {
            s.gun_weaponmodel = weaponmodel;
            s.gun = weapon_for_model(weaponmodel);
        }
        s.gun
    });
    let Some(id) = gun else {
        vm.model = std::ptr::null_mut();
        return;
    };
    let index = r::model_index(&shared::weapons::models(id).0);
    vm.model = spec::model_by_index(index);
    if vm.model.is_null() {
        return;
    }
    let items = state::with(|s| s.active_items.get(target as usize).copied().unwrap_or(0));
    vm.curstate.modelindex = index;
    vm.curstate.frame = 0.0;
    vm.curstate.colormap = 0;
    vm.curstate.body = crate::events::fire::weapon_body(id as i32, items as i32);
    vm.index = target;
    // `V_CalcGunAngle` with a bob global (`0x10191dac`) nothing writes, so 0.
    crate::view::calc_gun_angle(vm, p.viewangles, 0.0);
    let mut angles = vm.angles;
    let mut origin = p.vieworg;
    spec::apply_shake(&mut origin, &mut angles, 0.9);
    // The gun offset `V_CalcBob` last left in the normal view (`0x10069a70`).
    origin = v::add(origin, crate::view::gun_offset());
    origin[2] -= 1.0;
    vm.origin = origin;
    vm.angles = angles;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_eye_leaves_the_view_alone_without_a_target() {
        // `V_GetInEyePos` writes nothing when the entity, its `player` flag or the player-info
        // name is missing (`in_eye_position` passes `None` then).
        let (mut org, mut ang) = ([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]);
        in_eye_from(None, &mut org, &mut ang);
        assert_eq!((org, ang), ([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]));
        let mut e = cl_entity_t { origin: [10.0, 20.0, 30.0], angles: [1.0, 90.0, 0.0], player: 1, ..Default::default() };
        e.curstate.solid = SOLID_SLIDEBOX as i16;
        in_eye_from(Some(&e), &mut org, &mut ang);
        assert_eq!(org, [10.0, 20.0, 58.0]);
        assert_eq!(ang, [-std::f32::consts::PI, 90.0, 0.0]);
    }

    #[test]
    fn in_eye_target_tests() {
        // `V_GetInEyePos`'s tests: a player entity with a player-info name; solid isn't one.
        let mut e = cl_entity_t { origin: [1.0, 2.0, 3.0], angles: [4.0, 5.0, 6.0], player: 1, ..Default::default() };
        e.curstate.usehull = 1;
        let t = in_eye_target_of(Some(&e), || true).unwrap();
        assert_eq!((t.origin, t.angles, t.solid, t.usehull), ([1.0, 2.0, 3.0], [4.0, 5.0, 6.0], 0, 1));
        assert_eq!(in_eye_target_of(Some(&e), || false), None, "no name");
        e.player = 0;
        assert_eq!(in_eye_target_of(Some(&e), || true), None, "not a player");
        assert_eq!(in_eye_target_of(None, || true), None);
    }

    #[test]
    fn active_player_is_ts() {
        // `IsActivePlayer` (0x100236d0): player, solid, not local, named; the model isn't tested.
        let mut e = cl_entity_t { index: 3, player: 1, ..Default::default() };
        e.curstate.solid = SOLID_SLIDEBOX as i16;
        assert!(is_active_player(Some(&e), 1, || true), "a modelless solid player passes");
        assert!(!is_active_player(Some(&e), 1, || false), "a slot without a name doesn't");
        assert!(!is_active_player(Some(&e), 3, || true), "nor the local player");
        assert!(!is_active_player(None, 1, || true));
        e.curstate.solid = SOLID_NOT as i16;
        assert!(!is_active_player(Some(&e), 1, || true), "a dead player or spectator (SOLID_NOT) doesn't");
        e.curstate.solid = SOLID_SLIDEBOX as i16;
        e.player = 0;
        assert!(!is_active_player(Some(&e), 1, || true), "nor a non-player (the world for target 0)");
    }

    #[test]
    fn specmode_numbers() {
        let mut s = Spectator { last_primary: 7, last_secondary: 9, ..Default::default() };
        let mut st = state::Shared::default();
        let c = set_modes_in(&mut s, &mut st, 2, 0, |i| i == 7).unwrap();
        assert_eq!(st.spec_mode, obs::LOCKED_VIEW, "specmode 2 is the locked view (4)");
        assert_eq!((st.spec_target, st.spec_flags, s.locked_target), (7, 9, 7));
        assert_eq!(c.texts, ["#Spec_Mode2"]);
        assert_eq!(c.crosshair, Some(false));

        let c = set_modes_in(&mut s, &mut st, 5, 1, |_| true).unwrap();
        assert_eq!(st.spec_mode, obs::IN_EYE);
        assert_eq!(c.texts, ["#Spec_Mode_Inset1", "#Spec_Mode5"]);
        assert_eq!(c.crosshair, Some(true));

        let c = set_modes_in(&mut s, &mut st, 3, 1, |_| true).unwrap();
        assert_eq!((st.spec_mode, st.spec_target, s.locked_target), (obs::ROAMING, 0, 0));
        assert_eq!(c.crosshair, Some(true));

        let c = set_modes_in(&mut s, &mut st, 1, 1, |_| true).unwrap();
        assert_eq!(st.spec_mode, obs::ROAMING, "1 only prints");
        assert_eq!(c.texts, ["#Spec_Mode1"]);
        assert!(set_modes_in(&mut s, &mut st, 6, 1, |_| true).is_none());
    }

    #[test]
    fn map_camera_uses_origin_before_classname() {
        let kv = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let ents = vec![
            kv(&[("classname", "worldspawn")]),
            kv(&[("origin", "1 2 3"), ("classname", "info_player_deathmatch")]),
            kv(&[("origin", "10 20"), ("classname", "info_player_start")]),
            kv(&[("origin", "7 7 7"), ("classname", "info_player_start")]),
        ];
        assert_eq!(camera_from_entities(&ents), [10.0, 20.0, 0.0]);
    }

    #[test]
    fn rings() {
        let mut r = Ring::default();
        for i in 0..10 {
            r.push([i as f32, 0.0, 0.0], 1.0 + i as f32 * 0.1);
        }
        let (a, b, f) = r.sample(1.55).unwrap();
        assert_eq!((a[0], b[0]), (5.0, 6.0));
        assert!((f - 0.5).abs() < 1e-3);
        assert!(r.sample(0.5).is_none(), "history doesn't reach back");
        let (a, b) = r.pair(1.55).unwrap();
        assert!((r.angle_frac(a, b, 1.55).unwrap() - 0.5).abs() < 1e-3);
        assert_eq!(r.angle_frac(a, b, 9.0), Some(1.0), "capped at 1");
        r.time[b] = r.time[a];
        assert_eq!(r.angle_frac(a, b, 1.55), None, "no span");
        // The span and offset stay `double`s: one rounding at the end.
        r.time[a] = 1.1;
        r.time[b] = 1.3;
        let want = ((1.2f32 as f64 - 1.1f32 as f64) / (1.3f32 as f64 - 1.1f32 as f64)) as f32;
        assert_eq!(r.angle_frac(a, b, 1.2), Some(want));
    }

    #[test]
    fn interpolate_angles_is_ts() {
        let (mut a, mut b) = ([0.0, 170.0, 0.0], [0.0, -170.0, 0.0]);
        let i = interpolate_angles(&mut a, &mut b, 0.5);
        assert_eq!(i[1], 180.0, "blends across ±180; 180 stays 180");
        let (mut a, mut b) = ([0.0, -170.0, 0.0], [0.0, 170.0, 0.0]);
        assert_eq!(interpolate_angles(&mut a, &mut b, 0.5)[1], -180.0, "and -180 stays -180");
        // Both inputs are normalised in place, one step each (no modulo).
        let (mut a, mut b) = ([350.0, 600.0, -190.0], [10.0, 0.0, 0.0]);
        interpolate_angles(&mut a, &mut b, 0.0);
        assert_eq!(a, [-10.0, 240.0, 170.0]);
        // The difference isn't rounded to `float` before `× frac + start`.
        let (x, y, f) = (0.1f32, 123.456f32, 0.1f32);
        let want = ((y as f64 - x as f64) * f as f64 + x as f64) as f32;
        let (mut a, mut b) = ([x; 3], [y; 3]);
        assert_eq!(interpolate_angles(&mut a, &mut b, f), [want; 3]);
        // A NaN difference takes the `+ 360` branch and stays NaN.
        let (mut a, mut b) = ([f32::NAN, 0.0, 0.0], [0.0; 3]);
        assert!(interpolate_angles(&mut a, &mut b, 0.5)[0].is_nan());
    }

    #[test]
    fn angle_between_is_ts() {
        assert!((angle_between([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]) - 90.0).abs() < 1e-3);
        assert_eq!(angle_between([0.0; 3], [1.0, 0.0, 0.0]), 0.0);
        assert_eq!(angle_between([f32::NAN, 0.0, 0.0], [1.0, 0.0, 0.0]), 0.0, "a NaN length counts as 0");
        // The arc cosine comes before the division by the lengths.
        assert!((angle_between([2.0, 0.0, 0.0], [0.0, 2.0, 0.0]) - 22.5).abs() < 1e-4);
        // No clamp: past ±1 the arc cosine is NaN (which the smoothing test lets through).
        assert!(angle_between([2.0, 0.0, 0.0], [2.0, 0.0, 0.0]).is_nan());
        assert!(length([3.0, 4.0, 0.0]) == 5.0);
        assert!(x87_eq0(f64::NAN) && x87_eq0(0.0) && !x87_eq0(1e-30));
    }

    #[test]
    fn director_message_maps_new_commands() {
        assert_eq!(parse_director_message(&[2, 5, 0, 6, 0, 0x27, 0, 0, 0]), Some((director::DRC_CAMERA, 5, 6, 0x27)));
        assert_eq!(parse_director_message(&[1]), Some((director::DRC_ACTIVE, 0, 0, 0)));
        assert_eq!(parse_director_message(&[3, 1]), None, "DRC_CMD_MODE isn't handled by TS");
        assert_eq!(parse_director_message(&[]), None);
    }

    /// `svc_director` payloads and entity-lump origins of any length and content.
    #[test]
    fn director_and_origin_garbage() {
        let mut x = 0xdead_beef_u32;
        let mut rnd = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        assert_eq!(parse_director_message(&[2]), Some((director::DRC_CAMERA, 0, 0, 0)), "short: zeros");
        assert_eq!(parse_director_message(&[0xff; 64]), None);
        assert_eq!(
            parse_director_message(&[2, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            Some((director::DRC_CAMERA, 65535, 65535, -1))
        );
        for n in 0..3000 {
            let buf: Vec<u8> = (0..n % 20).map(|_| rnd() as u8 & if n % 3 == 0 { 3 } else { 0xff }).collect();
            parse_director_message(&buf);
            let text: String = buf.iter().map(|&b| if b < 0x80 { b as char } else { 'é' }).collect();
            parse_vec(&text);
        }
        let v = parse_vec("1e40 nan -inf x");
        assert!(v[0] == f32::INFINITY && v[1].is_nan() && v[2] == f32::NEG_INFINITY);
        let ents = vec![vec![("origin".to_string(), "é 2 3 4 5".to_string()), ("classname".to_string(), "info_player_start".to_string())]];
        assert_eq!(camera_from_entities(&ents), [0.0, 2.0, 3.0]);
    }
}
