//! The Specialists server library as the engine sees it: installed as dlls/ts_i386.so,
//! dlls/ts_amd64.so (FWGS 64-bit) and dlls/mp.dll.
//!
//! This crate holds every engine entry point and callback. It turns the engine's raw pointers
//! into references and calls the game logic in `ts-server`, which has no `unsafe` code. Keep
//! the functions here thin: conversion and routing only (see AGENTS.md).

// Every `unsafe` block states why it holds (see AGENTS.md).
#![warn(clippy::undocumented_unsafe_blocks)]
// The exports are the engine's ABI entry points: only the engine calls them, with the arguments its
// headers document, so they carry no Rust-side `# Safety` docs.
#![allow(clippy::missing_safety_doc)]

// A library with no ruleset would have nothing to register (gameplay-plugins.md §1).
#[cfg(not(any(feature = "ts30", feature = "ts21")))]
compile_error!("enable at least one gameplay ruleset feature: `ts30` or `ts21`");

mod exports;
mod plugin_api;
mod plugins;

use goldsrc::edict::EdictPtr;
use goldsrc::ext::saverestore::{EngineHost, SaveData, raw};
use goldsrc::server::{self as sv, engine};
use goldsrc::sys::*;
use goldsrc::util::cstr;
use std::ffi::{CStr, c_char, c_int, c_uchar, c_uint, c_void};
use ts_server::world::UseType;
use ts_server::{bot_backend, commands, entities, ham, messages, net, player, rules, save, stunt, weapons};

goldsrc::stack_aligned! {
    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn GiveFnptrsToDll(funcs: *const enginefuncs_t, globals: *mut globalvars_t) {
        // SAFETY: the engine passes its function table (and globals) here, before any other call into the library.
        unsafe { sv::init(funcs, globals) };
        goldsrc::panic::install("ts_server_panic.log", |m| engine().print(m));
    }
}

goldsrc::stack_aligned! {
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn GetEntityAPI2(table: *mut DLL_FUNCTIONS, ver: *mut c_int) -> c_int {
        // SAFETY: the engine passes valid pointers (and writable out-pointers) for the duration of this call.
        unsafe {
            if table.is_null() || *ver != INTERFACE_VERSION as c_int {
                *ver = INTERFACE_VERSION as c_int;
                return 0;
            }
            *table = dll_functions();
        }
        1
    }
}

goldsrc::stack_aligned! {
    /// Legacy entry point (interface version passed by value). Some engines try it first.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn GetEntityAPI(table: *mut DLL_FUNCTIONS, ver: c_int) -> c_int {
        if table.is_null() || ver != INTERFACE_VERSION as c_int {
            return 0;
        }
        // SAFETY: the engine passes valid out-pointers for this call.
        unsafe { *table = dll_functions() };
        1
    }
}

goldsrc::stack_aligned! {
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn GetNewDLLFunctions(table: *mut NEW_DLL_FUNCTIONS, ver: *mut c_int) -> c_int {
        // SAFETY: the engine passes valid pointers (and writable out-pointers) for the duration of this call.
        unsafe {
            if table.is_null() || *ver != NEW_DLL_FUNCTIONS_VERSION as c_int {
                *ver = NEW_DLL_FUNCTIONS_VERSION as c_int;
                return 0;
            }
            *table = NEW_DLL_FUNCTIONS {
                pfnOnFreeEntPrivateData: Some(on_free_private_data),
                pfnGameShutdown: Some(game_shutdown),
                pfnShouldCollide: Some(should_collide),
                ..Default::default()
            };
        }
        1
    }
}

/// Every slot is filled: GoldSrc calls most of them without null checks.
fn dll_functions() -> DLL_FUNCTIONS {
    DLL_FUNCTIONS {
        pfnGameInit: Some(game_init),
        pfnSpawn: Some(spawn),
        pfnThink: Some(think),
        pfnUse: Some(use_),
        pfnTouch: Some(touch),
        pfnBlocked: Some(blocked),
        pfnKeyValue: Some(key_value),
        pfnSave: Some(save),
        pfnRestore: Some(restore),
        pfnSetAbsBox: Some(set_abs_box),
        pfnSaveWriteFields: Some(save_write_fields),
        pfnSaveReadFields: Some(save_read_fields),
        pfnSaveGlobalState: Some(save_global_state),
        pfnRestoreGlobalState: Some(restore_global_state),
        pfnResetGlobalState: Some(reset_global_state),
        pfnClientConnect: Some(client_connect),
        pfnClientDisconnect: Some(client_disconnect),
        pfnClientKill: Some(client_kill),
        pfnClientPutInServer: Some(client_put_in_server),
        pfnClientCommand: Some(client_command),
        pfnClientUserInfoChanged: Some(client_userinfo_changed),
        pfnServerActivate: Some(server_activate),
        pfnServerDeactivate: Some(server_deactivate),
        pfnPlayerPreThink: Some(player_pre_think),
        pfnPlayerPostThink: Some(player_post_think),
        pfnStartFrame: Some(start_frame),
        pfnParmsNewLevel: Some(noop),
        pfnParmsChangeLevel: Some(parms_change_level),
        pfnGetGameDescription: Some(get_game_description),
        pfnPlayerCustomization: Some(player_customization),
        pfnSpectatorConnect: Some(noop_e),
        pfnSpectatorDisconnect: Some(noop_e),
        pfnSpectatorThink: Some(noop_e),
        pfnSys_Error: Some(sys_error),
        pfnPM_Move: Some(pm_move),
        pfnPM_Init: Some(pm_init),
        pfnPM_FindTextureType: Some(pm_find_texture_type),
        pfnSetupVisibility: Some(setup_visibility),
        pfnUpdateClientData: Some(update_client_data),
        pfnAddToFullPack: Some(add_to_full_pack),
        pfnCreateBaseline: Some(create_baseline),
        pfnRegisterEncoders: Some(register_encoders),
        pfnGetWeaponData: Some(get_weapon_data),
        pfnCmdStart: Some(cmd_start),
        pfnCmdEnd: Some(cmd_end),
        pfnConnectionlessPacket: Some(connectionless_packet),
        pfnGetHullBounds: Some(get_hull_bounds),
        pfnCreateInstancedBaselines: Some(noop),
        pfnInconsistentFile: Some(inconsistent_file),
        pfnAllowLagCompensation: Some(allow_lag_compensation),
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn noop() {}
}

goldsrc::stack_aligned! {
    /// `RegisterEncoders` (TS 0xb004c): binds `delta.lst`'s three conditional encoders.
    unsafe extern "C" fn register_encoders() {
        let e = engine();
        e.delta_add_encoder(c"Entity_Encode", entity_encode);
        e.delta_add_encoder(c"Custom_Encode", custom_encode);
        e.delta_add_encoder(c"Player_Encode", player_encode);
    }
}

/// Turns a conditional encoder's arguments into the delta handle and the two states, or `None`
/// for a null pointer.
///
/// # Safety
/// The engine's arguments to the running encoder: a delta description and two `entity_state_t`
/// (passed as bytes) valid for the call.
unsafe fn encoder_args<'a>(d: *mut delta_s, from: *const u8, to: *const u8) -> Option<(sv::Delta, &'a entity_state_t, &'a entity_state_t)> {
    if d.is_null() {
        return None;
    }
    // SAFETY: the engine passes the delta it is encoding with, and `from`/`to` point at the
    // entity states (or custom entity states, which share `entity_state_t`) for this call.
    unsafe { Some((sv::Delta::from_raw(d), from.cast::<entity_state_t>().as_ref()?, to.cast::<entity_state_t>().as_ref()?)) }
}

goldsrc::stack_aligned! {
    /// `Entity_Encode` (TS 0xafaa4).
    unsafe extern "C" fn entity_encode(d: *mut delta_s, from: *const u8, to: *const u8) {
        // SAFETY: called by the engine with its encoder arguments.
        if let Some((d, f, t)) = unsafe { encoder_args(d, from, to) } {
            ts_server::delta::entity_encode(d, f, t);
        }
    }
}

goldsrc::stack_aligned! {
    /// `Player_Encode` (TS 0xafcbc).
    unsafe extern "C" fn player_encode(d: *mut delta_s, from: *const u8, to: *const u8) {
        // SAFETY: called by the engine with its encoder arguments.
        if let Some((d, f, t)) = unsafe { encoder_args(d, from, to) } {
            ts_server::delta::player_encode(d, f, t);
        }
    }
}

goldsrc::stack_aligned! {
    /// `Custom_Encode` (TS 0xafec4).
    unsafe extern "C" fn custom_encode(d: *mut delta_s, from: *const u8, to: *const u8) {
        // SAFETY: called by the engine with its encoder arguments.
        if let Some((d, f, t)) = unsafe { encoder_args(d, from, to) } {
            ts_server::delta::custom_encode(d, f, t);
        }
    }
}

goldsrc::stack_aligned! {
    /// `ParmsChangeLevel` (TS 0xaea68): the transition list for the save the engine is writing.
    unsafe extern "C" fn parms_change_level() {
        entities::stock::levelchange::parms_change_level();
    }
}

goldsrc::stack_aligned! {
    /// `ResetGlobalState` (TS 0xf43fc).
    unsafe extern "C" fn reset_global_state() {
        save::reset_global_state();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn noop_e(_e: *mut edict_t) {}
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn game_init() {
        // TS registers `VoiceMask`/`ReqState` in the `CHalfLifeMultiplay` constructor, before
        // `LinkUserMessages` registers the rest, so they take the first two message numbers.
        rules::voice::register_messages();
        messages::register_all();
        // Before the rules' `game_init`: `ts_gameplay`'s default is the default ruleset's id, and the
        // plugins' own cvars follow the server's.
        register_gameplay();
        rules::game_init();
        weapons::game_init();
        stunt::game_init();
        // The bot backends: TS's own (native) first, then the `tsmod_*` plugins' (issue #104).
        bot_backend::game_init();
        engine().print("[ts] GameInit: Rust TS server library loaded\n");
        plugins::load_all();
        ts_server::plugins::game_init();
    }
}

/// The gameplay plugins (`docs/implementation/gameplay-plugins.md`), each with its native bot
/// rules (the bot layer's own registry, docs/implementation/bot-plugins.md §2.2): TS 3.0 with the
/// `ts30` feature, then TS 2.1 with the `ts21` feature. The first registered is the default.
fn register_gameplay() {
    #[cfg(feature = "ts30")]
    {
        ts_server::gameplay::register(ts30_gameplay::ID, &ts30_server::PLUGIN);
        ts_server::bots::rules::register(ts30_gameplay::ID, &ts30_server::bots::BOTS);
    }
    #[cfg(feature = "ts21")]
    {
        ts_server::gameplay::register(ts21_gameplay::ID, &ts21_server::PLUGIN);
        ts_server::bots::rules::register(ts21_gameplay::ID, &ts21_server::bots::BOTS);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn spawn(e: *mut edict_t) -> c_int {
        level_start_on_world(e);
        entities::dispatch_spawn(e)
    }
}

/// The world (edict 0) is spawned or restored first in every level: the engine's precache tables
/// are empty again and take new names (`goldsrc::guard`).
fn level_start_on_world(e: *mut edict_t) {
    if !e.is_null() && engine().index_of(e) == 0 {
        goldsrc::guard::level_loading();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn think(e: *mut edict_t) {
        ham::think(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn touch(e: *mut edict_t, other: *mut edict_t) {
        ham::touch(e, other);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn use_(e: *mut edict_t, other: *mut edict_t) {
        ham::use_(e, other, other, UseType::Toggle, 0.0);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn blocked(e: *mut edict_t, other: *mut edict_t) {
        ham::blocked(e, other);
    }
}

goldsrc::stack_aligned! {
    /// `DispatchKeyValue` (0xaad4c): entvars fields first, then the class (`entities::dispatch_key_value`).
    /// The engine doesn't parse any keyvalues itself and requires `classname` to be handled.
    unsafe extern "C" fn key_value(e: *mut edict_t, kvd: *mut KeyValueData) {
        if e.is_null() || kvd.is_null() {
            return;
        }
        // SAFETY: non-null, and the engine keeps the KeyValueData valid for the duration of this call.
        let kvd = unsafe { &mut *kvd };
        // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
        let (key, value) = unsafe { (cstr(kvd.szKeyName), cstr(kvd.szValue)) };
        // SAFETY: as above.
        let class = (!kvd.szClassName.is_null()).then(|| unsafe { cstr(kvd.szClassName) });
        kvd.fHandled = entities::dispatch_key_value(e, class, key, value) as c_int;
    }
}

/// `DispatchSave` (TS 0xaaf18).
// NOTE: `save` is wrapped manually (not with `stack_aligned!`) because the macro creates
// `mod save` on i686 Linux, which would shadow the `use ts_server::save` import at crate root.
// The body module uses `_save_body` to avoid the conflict.
#[cfg(all(target_os = "linux", target_arch = "x86"))]
#[doc(hidden)]
mod _save_body {
    #[allow(unused_imports)]
    use super::*;
    #[inline(never)]
    pub(super) unsafe extern "C" fn body(e: *mut edict_t, sd: *mut SAVERESTOREDATA) {
        // SAFETY: the engine passes its live save state (or null) for the duration of this call.
        if let Some(mut sd) = unsafe { SaveData::from_raw(sd) } {
            save::dispatch_save(e, &mut sd);
        }
    }
}
#[cfg(all(target_os = "linux", target_arch = "x86"))]
#[unsafe(naked)]
unsafe extern "C" fn save(_e: *mut edict_t, _sd: *mut SAVERESTOREDATA) {
    goldsrc::_stack_align_trampoline!(_save_body)
}
#[cfg(not(all(target_os = "linux", target_arch = "x86")))]
unsafe extern "C" fn save(e: *mut edict_t, sd: *mut SAVERESTOREDATA) {
    // SAFETY: the engine passes its live save state (or null) for the duration of this call.
    if let Some(mut sd) = unsafe { SaveData::from_raw(sd) } {
        save::dispatch_save(e, &mut sd);
    }
}

goldsrc::stack_aligned! {
    /// `DispatchRestore` (TS 0xab128).
    unsafe extern "C" fn restore(e: *mut edict_t, sd: *mut SAVERESTOREDATA, global_entity: c_int) -> c_int {
        level_start_on_world(e);
        // SAFETY: as in `save`.
        match unsafe { SaveData::from_raw(sd) } {
            Some(mut sd) => save::dispatch_restore(e, &mut sd, global_entity != 0),
            None => 0,
        }
    }
}

goldsrc::stack_aligned! {
    /// `SaveWriteFields` (TS 0xab654): the engine's own structs (save header, entity table, ...).
    unsafe extern "C" fn save_write_fields(
        sd: *mut SAVERESTOREDATA,
        name: *const c_char,
        base: *mut c_void,
        descs: *mut TYPEDESCRIPTION,
        n: c_int,
    ) {
        if name.is_null() || base.is_null() {
            return;
        }
        // SAFETY: the engine passes its save state, a static block name, the struct and its `n`
        // static descriptors, valid for this call (the names outlive the save as tokens).
        unsafe {
            let Some(mut sd) = SaveData::from_raw(sd) else { return };
            let name: &'static CStr = CStr::from_ptr(name);
            let host = EngineHost;
            let fields = raw::descs(descs, n);
            let descs: Vec<_> = fields.iter().map(|(d, _)| *d).collect();
            let vals: Vec<_> = fields.iter().map(|(d, off)| raw::load(&host, base.cast(), d, *off)).collect();
            sd.write_fields(&host, name, &descs, &vals);
        }
    }
}

goldsrc::stack_aligned! {
    /// `SaveReadFields` (TS 0xab6e8): reads them back (`m_global` off, `m_precache` on).
    unsafe extern "C" fn save_read_fields(
        sd: *mut SAVERESTOREDATA,
        name: *const c_char,
        base: *mut c_void,
        descs: *mut TYPEDESCRIPTION,
        n: c_int,
    ) {
        if name.is_null() || base.is_null() {
            return;
        }
        // SAFETY: as in `save_write_fields`, with `base` writable.
        unsafe {
            let Some(mut sd) = SaveData::from_raw(sd) else { return };
            let name: &'static CStr = CStr::from_ptr(name);
            let host = EngineHost;
            let fields = raw::descs(descs, n);
            let descs: Vec<_> = fields.iter().map(|(d, _)| *d).collect();
            let Some(out) = sd.read_fields(&host, name, &descs, false, true) else { return };
            for ((d, off), v) in fields.iter().zip(out) {
                if let Some(v) = v {
                    raw::store(base.cast(), d, *off, &v);
                }
            }
        }
    }
}

goldsrc::stack_aligned! {
    /// `SaveGlobalState` (TS 0xf42ec).
    unsafe extern "C" fn save_global_state(sd: *mut SAVERESTOREDATA) {
        // SAFETY: as in `save`.
        if let Some(mut sd) = unsafe { SaveData::from_raw(sd) } {
            save::save_global_state(&mut sd);
        }
    }
}

goldsrc::stack_aligned! {
    /// `RestoreGlobalState` (TS 0xf436c).
    unsafe extern "C" fn restore_global_state(sd: *mut SAVERESTOREDATA) {
        // SAFETY: as in `save`.
        if let Some(mut sd) = unsafe { SaveData::from_raw(sd) } {
            save::restore_global_state(&mut sd);
        }
    }
}

goldsrc::stack_aligned! {
    /// `DispatchObjectCollsionBox`: the class's collision box (`entities::set_object_collision_box`).
    unsafe extern "C" fn set_abs_box(e: *mut edict_t) {
        entities::set_object_collision_box(e);
    }
}

/// The engines' `szRejectReason` buffer (FWGS `SV_New_f`, ReHLDS `SV_ConnectClient_internal`,
/// HLDS): 128 bytes.
const REJECT_REASON_SIZE: usize = 128;

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_connect(e: *mut edict_t, name: *const c_char, addr: *const c_char, reject: *mut c_char) -> qboolean {
        // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
        let (name, addr) = unsafe { (cstr(name), cstr(addr)) };
        match rules::client_connect(e, name, addr) {
            Ok(()) => 1,
            Err(reason) => {
                if !reject.is_null() {
                    let n = reason.len().min(REJECT_REASON_SIZE - 1);
                    // SAFETY: every engine passes its 128-byte `szRejectReason` for the game to fill;
                    // at most 127 bytes and the NUL are written, and `reason` doesn't overlap it.
                    unsafe {
                        std::ptr::copy_nonoverlapping(reason.as_ptr(), reject.cast::<u8>(), n);
                        *reject.add(n) = 0;
                    }
                }
                0
            }
        }
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_disconnect(e: *mut edict_t) {
        rules::client_disconnect(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_kill(e: *mut edict_t) {
        rules::client_kill(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_put_in_server(e: *mut edict_t) {
        player::client_put_in_server(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_command(e: *mut edict_t) {
        commands::client_command(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn client_userinfo_changed(e: *mut edict_t, _infobuffer: *mut c_char) {
        rules::userinfo_changed(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn server_activate(_edicts: *mut edict_t, edict_count: c_int, client_max: c_int) {
        rules::server_activate(edict_count, client_max);
        engine().print(&format!("[ts] ServerActivate: {edict_count} edicts, {client_max} max clients\n"));
        // The engine goes `ss_active` when this returns and refuses new precaches from then on.
        goldsrc::guard::level_active();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn server_deactivate() {
        rules::server_deactivate();
        goldsrc::guard::level_loading();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn player_pre_think(e: *mut edict_t) {
        ham::player_pre_think(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn player_post_think(e: *mut edict_t) {
        ham::player_post_think(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn start_frame() {
        // mp.dll's `pfnStartFrame` (0x10010e00) thinks the bots before the rules' `Think`.
        bot_backend::start_frame();
        // Port addition: the `tsmod_*` plugins' frame (their bots run here, as the native ones do).
        ts_server::plugins::start_frame();
        rules::start_frame();
        // Port addition: the plugin API's per-frame snapshot of every player.
        ts_server::plugin_api::start_frame();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn get_game_description() -> *const c_char {
        rules::game_description().as_ptr()
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn player_customization(e: *mut edict_t, custom: *mut customization_t) {
        // SAFETY: the engine passes null or a customization valid for this call.
        let custom = unsafe { custom.as_ref() }.map(|c| (c.resource.type_ as i32, c.nUserData2));
        player::impulse::player_customization(e, custom);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn sys_error(msg: *const c_char) {
        // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
        eprintln!("[ts] Sys_Error: {}", unsafe { cstr(msg) }.to_string_lossy());
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn pm_move(pm: *mut playermove_t, server: qboolean) {
        // SAFETY: the engine passes valid pointers (null-checked where it may pass null) for the duration of this call.
        let pm = unsafe { &mut *pm };
        // TS's `PM_Move` (0x101188) hands `server != 0` to `PM_PlayerMove`, which stores it.
        pm.server = (server != 0) as qboolean;
        let gameplay = ts_server::gameplay::pm_rules(pm);
        shared::pm::player_move(pm, rules::cvars::pm_arith(), gameplay);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn pm_init(pm: *mut playermove_t) {
        // SAFETY: the engine passes valid pointers (null-checked where it may pass null) for the duration of this call.
        shared::pm::init(unsafe { &mut *pm });
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn pm_find_texture_type(name: *mut c_char) -> c_char {
        // SAFETY: the engine passes null or a NUL-terminated string valid for this call.
        shared::pm::find_texture_type(unsafe { cstr(name) }) as c_char
    }
}

goldsrc::stack_aligned! {
    /// `SetupVisibility` (0xaf030): the view origin's PVS/PAS decides which entities this client may
    /// see and hear; NULL sets for an HLTV proxy (`net::visibility_origin`).
    unsafe extern "C" fn setup_visibility(view: *mut edict_t, client: *mut edict_t, pvs: *mut *mut c_uchar, pas: *mut *mut c_uchar) {
        let org = net::visibility_origin(client.v().flags, (if view.is_null() { client } else { view }).v());
        // SAFETY: the engine passes valid pointers (and writable out-pointers) for the duration of this call.
        unsafe {
            (*pvs, *pas) = match org {
                Some(org) => (goldsrc::eng!(pfnSetFatPVS(org.as_ptr())), goldsrc::eng!(pfnSetFatPAS(org.as_ptr()))),
                None => (std::ptr::null_mut(), std::ptr::null_mut()),
            };
        }
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn update_client_data(ent: *const edict_t, _sendweapons: c_int, cd: *mut clientdata_t) {
        // SAFETY: the engine passes a valid clientdata_t for this call.
        net::update_client_data(ent, unsafe { &mut *cd });
    }
}

goldsrc::stack_aligned! {
    #[allow(clippy::too_many_arguments)]
    unsafe extern "C" fn add_to_full_pack(
        state: *mut entity_state_t,
        e: c_int,
        ent: *mut edict_t,
        host: *mut edict_t,
        hostflags: c_int,
        player: c_int,
        pset: *mut u8,
    ) -> c_int {
        // SAFETY: the engine passes a valid entity_state_t to fill.
        net::add_to_full_pack(unsafe { &mut *state }, e, ent, host, hostflags, player, pset)
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn create_baseline(
        player: c_int,
        eindex: c_int,
        baseline: *mut entity_state_t,
        entity: *mut edict_t,
        playermodelindex: c_int,
        player_mins: *mut f32,
        player_maxs: *mut f32,
    ) {
        // SAFETY: the engine passes a valid baseline and, for players, two vec3_t hull bounds.
        let (b, mins, maxs) = unsafe {
            let hull = |p: *mut f32| if p.is_null() { [0.0; 3] } else { *p.cast::<[f32; 3]>() };
            (&mut *baseline, hull(player_mins), hull(player_maxs))
        };
        net::create_baseline(player, eindex, b, entity, playermodelindex, mins, maxs);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn get_weapon_data(e: *mut edict_t, info: *mut weapon_data_t) -> c_int {
        // SAFETY: the engine passes valid pointers (null-checked where it may pass null) for the duration of this call.
        let info = unsafe { &mut *info.cast::<[weapon_data_t; 32]>() };
        player::with_player(e, |p, cx| weapons::get_weapon_data(p, cx, info)).unwrap_or_else(|| {
            *info = [weapon_data_t::default(); 32];
            1
        })
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn cmd_start(e: *const edict_t, cmd: *const usercmd_t, seed: c_uint) {
        // SAFETY: the engine passes valid pointers (null-checked where it may pass null) for the duration of this call.
        let cmd = unsafe { &*cmd };
        net::cmd_start(e);
        player::with_player(e.cast_mut(), |p, cx| weapons::cmd_start(p, cx, cmd, seed));
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn cmd_end(e: *const edict_t) {
        net::cmd_end(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn connectionless_packet(_addr: *const netadr_s, _args: *const c_char, _buf: *mut c_char, size: *mut c_int) -> c_int {
        // SAFETY: the engine passes valid out-pointers for this call.
        unsafe { *size = 0 };
        0
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn get_hull_bounds(hull: c_int, mins: *mut f32, maxs: *mut f32) -> c_int {
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
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn inconsistent_file(_player: *const edict_t, _filename: *const c_char, _disconnect_message: *mut c_char) -> c_int {
        0
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn allow_lag_compensation() -> c_int {
        0
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn on_free_private_data(e: *mut edict_t) {
        ham::on_free_private_data(e);
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn game_shutdown() {
        ts_server::plugins::shutdown();
    }
}

goldsrc::stack_aligned! {
    unsafe extern "C" fn should_collide(touched: *mut edict_t, other: *mut edict_t) -> c_int {
        ham::should_collide(touched, other) as c_int
    }
}
