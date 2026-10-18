//! The Specialists client library as the engine sees it: installed as cl_dlls/client.dll,
//! client.so (i686) and client_amd64.so (FWGS 64-bit). Exports the individual `HUD_*`/`IN_*`/...
//! names, which both Steam GoldSrc (pre- and post-anniversary) and FWGS accept.
//!
//! This crate holds every engine entry point and callback. It turns the engine's raw pointers
//! into references and calls the game logic in `ts-client`, which has no `unsafe` code. Keep the
//! functions here thin: conversion and routing only (see AGENTS.md). The one exception is
//! `tempents`, the SDK temp-entity loop, which walks the engine's linked list.

// Every `unsafe` block states why it holds (see AGENTS.md).
#![warn(clippy::undocumented_unsafe_blocks)]
// The exports are the engine's ABI entry points: only the engine calls them, with the arguments its
// headers document, so they carry no Rust-side `# Safety` docs.
#![allow(clippy::missing_safety_doc)]

// A library with no ruleset would have nothing to register (gameplay-plugins.md §1).
#[cfg(not(any(feature = "ts30", feature = "ts21")))]
compile_error!("enable at least one gameplay ruleset feature: `ts30` or `ts21`");

mod ext;
mod tempents;
mod vgui;

use goldsrc::client::{self as cl, engine};
use goldsrc::sys::*;
use std::ffi::{c_char, c_int, c_void};
use ts_client::{effects, events, hud, input, menus, prediction, spectator, view, voice};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Initialize(funcs: *mut cl_enginefunc_t, version: c_int) -> c_int {
    if funcs.is_null() || version != CLDLL_INTERFACE_VERSION as c_int {
        return 0;
    }
    // SAFETY: the engine passes its function table (and globals) here, before any other call into the library.
    unsafe { cl::init(funcs) };
    goldsrc::panic::install("ts_client_panic.log", |m| engine().print(m));
    register_gameplay();
    // Port additions (issues #293, #297): verify the client plugins and ask about new or changed
    // ones, install the approval gate, then load what it approves, before `HUD_Init`.
    ts_client::plugins::install();
    ext::load_all();
    1
}

/// The gameplay plugins (`docs/implementation/gameplay-plugins.md`): TS 3.0 with the `ts30`
/// feature, then TS 2.1 with the `ts21` feature. The first registered is the default.
fn register_gameplay() {
    #[cfg(feature = "ts30")]
    ts_client::gameplay::register(ts30_gameplay::ID, &ts30_client::PLUGIN);
    #[cfg(feature = "ts21")]
    ts_client::gameplay::register(ts21_gameplay::ID, &ts21_client::PLUGIN);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Init() {
    input::init();
    hud::init();
    menus::init();
    voice::init();
    events::init();
    view::init();
    effects::init();
    spectator::init();
    ts_client::relaunch::init();
    engine().print("[ts] client: Rust TS client library initialized\n");
    ts_client::plugins::init();
    ts_client::ext::init();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_VidInit() -> c_int {
    hud::vid_init();
    // TS: VGui_Startup's viewport Initialize; the gun is spawned again on the next prediction.
    prediction::vid_init();
    voice::vid_init();
    effects::vid_init();
    spectator::vid_init();
    ts_client::ext::vid_init();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Redraw(time: f32, intermission: c_int) -> c_int {
    hud::redraw(time, intermission != 0);
    spectator::redraw();
    // The voice panels paint with the viewport's (`menus`), in VGUI1's child order.
    menus::redraw(time);
    // Port addition: an open offer card or the plugins panel (client-plugins.md §7.3, §8) is drawn
    // last, and while one is up the extensions get no redraw, so nothing covers it.
    if !ts_client::plugins::covers() {
        ts_client::ext::redraw(time, intermission != 0);
    }
    ts_client::plugins::redraw();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_UpdateClientData(cdata: *mut c_void, _time: f32) -> c_int {
    // SAFETY: the engine passes its client_data_t (layout mirrored by render::ClientData) or null.
    let Some(cd) = (unsafe { cdata.cast::<goldsrc::ext::render::ClientData>().as_mut() }) else { return 0 };
    // SDK order: IN_Commands (joystick buttons) first.
    input::commands();
    // Returning 1 tells the engine client_data_t changed (the eased zoom FOV).
    view::update_client_data(cd) as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Reset() {
    hud::reset();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Shutdown() {
    ts_client::ext::shutdown();
    // TS's `HUD_Shutdown` (`0x10027250`) jumps to `ShutdownInput` (`0x10027240`).
    input::shutdown();
    // TS's `g_VoiceStatus` destructor writes `voice_ban.dt` when the library unloads.
    voice::shutdown();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_PlayerMove(pm: *mut playermove_t, server: c_int) {
    // SAFETY: the engine passes its playermove_t (or null) for the duration of this call.
    let Some(pm) = (unsafe { pm.as_mut() }) else { return };
    // TS (`0x1002e990` -> `0x1002e330`) stores `server != 0` in `pmove->server` first.
    pm.server = (server != 0) as c_int;
    spectator::begin_player_move(pm);
    let rules = ts_client::gameplay::pm_rules(pm);
    shared::pm::player_move_with(pm, shared::pm::Arith::CLIENT, rules, Some(spectator::spectator_move));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_PlayerMoveInit(pm: *mut playermove_t) {
    // SAFETY: the engine passes its playermove_t (or null) for the duration of this call.
    if let Some(pm) = unsafe { pm.as_mut() } {
        shared::pm::init(pm);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_PlayerMoveTexture(name: *mut c_char) -> c_char {
    // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
    shared::pm::find_texture_type(unsafe { goldsrc::util::cstr(name) }) as c_char
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_GetHullBounds(hull: c_int, mins: *mut f32, maxs: *mut f32) -> c_int {
    if mins.is_null() || maxs.is_null() {
        return 0;
    }
    match shared::pm::hull_bounds(hull) {
        Some((lo, hi)) => {
            // SAFETY: the engine passes valid pointers (and writable out-pointers) for the duration of this call.
            unsafe {
                mins.cast::<[f32; 3]>().write(lo);
                maxs.cast::<[f32; 3]>().write(hi);
            }
            1
        }
        None => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IN_ActivateMouse() {
    input::activate_mouse();
    ts_client::ext::mouse_active(true);
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn IN_DeactivateMouse() {
    input::deactivate_mouse();
    ts_client::ext::mouse_active(false);
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn IN_MouseEvent(mstate: c_int) {
    input::mouse_event(mstate);
}
/// FWGS extension export: relative mouse look (degrees, already scaled by m_yaw/m_pitch).
#[unsafe(no_mangle)]
pub extern "C" fn IN_ClientLookEvent(relyaw: f32, relpitch: f32) {
    input::look_event(relyaw, relpitch);
}

/// FWGS extension export: joystick/touch movement (-1..1).
#[unsafe(no_mangle)]
pub extern "C" fn IN_ClientMoveEvent(forwardmove: f32, sidemove: f32) {
    input::move_event(forwardmove, sidemove);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn IN_ClearStates() {
    input::clear_states();
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn IN_Accumulate() {
    input::accumulate();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CL_CreateMove(frametime: f32, cmd: *mut usercmd_t, active: c_int) {
    // SAFETY: the engine passes its usercmd_t (or null) for the duration of this call.
    if let Some(cmd) = unsafe { cmd.as_mut() } {
        input::create_move(frametime, cmd, active != 0);
        ts_client::ext::usercmd(cmd, active != 0);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CL_IsThirdPerson() -> c_int {
    view::is_third_person() as c_int
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CL_CameraOffset(ofs: *mut f32) {
    if ofs.is_null() {
        return;
    }
    // SAFETY: the engine passes a valid out-pointer for this call (null-checked).
    unsafe { ofs.cast::<[f32; 3]>().write(view::camera_offset()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn KB_Find(name: *const c_char) -> *mut c_void {
    // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
    input::find(unsafe { goldsrc::util::cstr(name) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn CAM_Think() {
    view::cam_think();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn V_CalcRefdef(p: *mut ref_params_t) {
    // SAFETY: the engine passes its ref_params_t (or null) for the duration of this call.
    if let Some(p) = unsafe { p.as_mut() } {
        ts_client::ext::view::note_refdef(p.onground != 0);
        view::calc_refdef(p);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_AddEntity(kind: c_int, ent: *mut cl_entity_t, _model: *const c_char) -> c_int {
    // SAFETY: the engine passes the entity being added (or null).
    match unsafe { ent.as_mut() } {
        Some(e) => effects::add_entity(kind, e) as c_int,
        None => 1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_CreateEntities() {
    effects::create_entities();
    voice::create_entities();
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_DrawNormalTriangles() {
    effects::draw_normal();
    spectator::draw_overview();
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_DrawTransparentTriangles() {
    effects::draw_transparent();
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_StudioEvent(event: *const c_void, ent: *const cl_entity_t) {
    // SAFETY: the engine passes the mstudioevent_t being played and its entity (or nulls).
    if let (Some(ev), Some(ent)) = unsafe { (event.cast::<mstudioevent_s>().as_ref(), ent.as_ref()) } {
        effects::studio_event(ev, ent);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_PostRunCmd(
    from: *mut local_state_t,
    to: *mut local_state_t,
    cmd: *mut usercmd_t,
    runfuncs: c_int,
    time: f64,
    random_seed: u32,
) {
    // SAFETY: the engine passes valid pointers (or nulls, rejected here) for the duration of this call.
    let (Some(from), Some(to), Some(cmd)) = (unsafe { (from.as_mut(), to.as_mut(), cmd.as_mut()) }) else { return };
    prediction::post_run_cmd(from, to, cmd, runfuncs != 0, time, random_seed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_TxferLocalOverrides(state: *mut entity_state_t, client: *const clientdata_t) {
    // SAFETY: the engine passes valid pointers (or nulls, rejected here) for the duration of this call.
    if let (Some(state), Some(client)) = unsafe { (state.as_mut(), client.as_ref()) } {
        prediction::txfer_local_overrides(state, client);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_ProcessPlayerState(dst: *mut entity_state_t, src: *const entity_state_t) {
    // SAFETY: the engine passes valid pointers (or nulls, rejected here) for the duration of this call.
    if let (Some(dst), Some(src)) = unsafe { (dst.as_mut(), src.as_ref()) } {
        prediction::process_player_state(dst, src);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_TxferPredictionData(
    ps: *mut entity_state_t,
    pps: *const entity_state_t,
    pcd: *mut clientdata_t,
    ppcd: *const clientdata_t,
    wd: *mut weapon_data_t,
    pwd: *const weapon_data_t,
) {
    // SAFETY: the engine passes valid pointers (or nulls, rejected here) for the duration of this
    // call; the weapon data arrays have 32 entries (`MAX_WEAPONS`).
    let (Some(ps), Some(pps), Some(pcd), Some(ppcd), Some(wd), Some(pwd)) = (unsafe {
        (
            ps.as_mut(),
            pps.as_ref(),
            pcd.as_mut(),
            ppcd.as_ref(),
            wd.cast::<[weapon_data_t; 32]>().as_mut(),
            pwd.cast::<[weapon_data_t; 32]>().as_ref(),
        )
    }) else {
        return;
    };
    prediction::txfer_prediction_data(ps, pps, pcd, ppcd, wd, pwd);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn Demo_ReadBuffer(size: c_int, buf: *mut u8) {
    if buf.is_null() || size <= 0 {
        return;
    }
    // SAFETY: during playback the engine passes the `size`-byte record it read from the demo.
    let b = unsafe { std::slice::from_raw_parts(buf, size as usize) };
    ts_client::demo::read_buffer(b);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_ConnectionlessPacket(
    _from: *const c_void,
    _args: *const c_char,
    _buf: *mut c_char,
    size: *mut c_int,
) -> c_int {
    // SAFETY: the engine passes a valid out-pointer (or null) for this call.
    if let Some(size) = unsafe { size.as_mut() } {
        *size = 0;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Frame(time: f64) {
    // TS order: server browser, CVoiceStatus::Frame, emitters, slow motion, then the key gate.
    voice::frame(time);
    view::frame(time);
    hud::frame(time);
    input::frame();
    ts_client::ext::frame(time);
    // Port addition: server offers of client plugins (workers' results, the next card).
    ts_client::plugins::offers::frame();
    // Port addition: the next start's "Reconnect to <server>?" (client-plugins.md §13.5).
    ts_client::relaunch::frame();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_Key_Event(down: c_int, key: c_int, binding: *const c_char) -> c_int {
    // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
    let binding = unsafe { goldsrc::util::cstr(binding) };
    // Port addition: an open offer card takes Escape and Enter ("Not now", never an approval)
    // before anything else, extensions included (client-plugins.md §7.3); the plugins panel takes
    // Escape (close) and its paging keys.
    if ts_client::plugins::key_event(down != 0, key) {
        return 0;
    }
    // Port addition: client extensions see the key first, and may swallow it (issue #293).
    if ts_client::ext::key_event(down != 0, key, binding) {
        return 0;
    }
    input::hud_key_event(down != 0, key, binding)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_TempEntUpdate(
    frametime: f64,
    client_time: f64,
    gravity: f64,
    free: *mut *mut c_void,
    active: *mut *mut c_void,
    add_visible: *mut c_void,
    play_sound: *mut c_void,
) {
    if add_visible.is_null() || play_sound.is_null() {
        return;
    }
    // SAFETY: the engine passes its temp-entity free/active list heads and two callbacks with
    // these signatures (SDK `HUD_TempEntUpdate`).
    unsafe {
        let add: tempents::AddVisible = std::mem::transmute(add_visible);
        let snd: tempents::PlaySound = std::mem::transmute(play_sound);
        let ts = ts_client::state::with(|s| s.time_scale);
        tempents::update(frametime, client_time, gravity, free.cast(), active.cast(), add, snd, ts);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_GetUserEntity(_index: c_int) -> *mut c_void {
    std::ptr::null_mut()
}

/// The TS studio renderer's interface (`0x10093700`): `{1, R_StudioDrawModel, R_StudioDrawPlayer}`.
static STUDIO_INTERFACE: r_studio_interface_s = r_studio_interface_s {
    version: STUDIO_INTERFACE_VERSION as c_int,
    StudioDrawModel: Some(studio_draw_model),
    StudioDrawPlayer: Some(studio_draw_player),
};

/// `R_StudioDrawModel` (`0x1001f3d0`).
unsafe extern "C" fn studio_draw_model(flags: c_int) -> c_int {
    ts_client::studio::draw_model(flags) as c_int
}

/// `R_StudioDrawPlayer` (`0x1001f3b0`).
unsafe extern "C" fn studio_draw_player(flags: c_int, pplayer: *mut entity_state_s) -> c_int {
    if pplayer.is_null() {
        return 0;
    }
    // The renderer works on a copy (the engine's state could be the current entity's curstate,
    // which the renderer also writes) and writes back the two fields it changes.
    // SAFETY: the engine passes a valid player state for the duration of this call (null-checked).
    let mut st = unsafe { *pplayer };
    let r = ts_client::studio::draw_player(flags, &mut st);
    // SAFETY: as above; plain field writes, no reference kept.
    unsafe {
        (*pplayer).renderamt = st.renderamt;
        (*pplayer).gaitsequence = st.gaitsequence;
    }
    r as c_int
}

/// `HUD_GetStudioModelInterface` (`0x1001f3f0`): version 1 only; keeps the engine's studio API and
/// installs the TS renderer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_GetStudioModelInterface(
    version: c_int,
    ppinterface: *mut *mut r_studio_interface_s,
    pstudio: *mut engine_studio_api_s,
) -> c_int {
    if version != STUDIO_INTERFACE_VERSION as c_int || ppinterface.is_null() {
        return 0;
    }
    // SAFETY: the engine passes its studio API table (or null, rejected by `init`).
    if !unsafe { goldsrc::ext::studio::init(pstudio) } {
        return 0;
    }
    // SAFETY: `ppinterface` is the engine's out-pointer (null-checked). The engine only reads the
    // interface, which is a static that lives for the process.
    unsafe { *ppinterface = (&raw const STUDIO_INTERFACE).cast_mut() };
    ts_client::studio::init();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_VoiceStatus(entindex: c_int, talking: c_int) {
    voice::update_speaker_status(entindex, talking != 0);
}

/// SDK 2.3+ engines pass the `svc_director` payload here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_DirectorMessage(size: c_int, buf: *mut c_void) {
    if buf.is_null() || size <= 0 {
        return;
    }
    // SAFETY: the engine passes a buffer of `size` bytes (null-checked above), valid for this call.
    let bytes = unsafe { std::slice::from_raw_parts(buf.cast::<u8>(), size as usize) };
    spectator::director_message(bytes);
}

/// TS's export (`0x1001ab60`), the pre-SDK-2.3 director interface. Current engines don't call it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_DirectorEvent(command: u8, first: u32, second: u32, flags: u32) {
    spectator::director_event(command, first as i32, second as i32, flags as i32);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn HUD_ChatInputPosition(_x: *mut c_int, _y: *mut c_int) {}
