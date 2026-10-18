//! Engine helpers added by the hud work package. Keep additions here to avoid merge conflicts.
//!
//! Only slots present in the 2007 `cl_enginefunc_t` that TS 3.0 was built against are used, so
//! these work on old Steam GoldSrc, HL25 and FWGS alike. The one exception,
//! [`ClEngine::fill_blend`], checks for an engine that has its slot and falls back otherwise.
//!
//! Each engine call has one wrapper, either here or in [`render`]; the `ClEngine` table
//! accessors (`event_api`, `efx_api`, `tri_api`, `demo_api`) are the null-checked ones, and
//! the `ev!`/`efx!`/`tri!` macros go through them.
//!
//! # Client entities
//! Game code reaches a `cl_entity_t` by index only through [`ClEngine::entity`] and
//! [`ClEngine::local_player`], which hand out shared `&'static` references, and
//! [`ClEngine::with_entity_mut`] / [`ClEngine::with_local_player_mut`], which lend a `&mut`
//! for the length of a closure. The engine owns the entity array for the process (slots are
//! reused, never unmapped) and rewrites entries each frame from the network, all on the client
//! thread. So, as with edicts (`edict.rs`):
//! * read what you need and drop the shared reference within the callback; don't keep it
//!   across frames, since the slot may have been reused for another entity;
//! * never hold a shared reference to an entity across a `with_*_mut` call (or a call that
//!   makes the engine rewrite entities, such as `EV_SetUpPlayerPrediction`, which recomputes
//!   other players' origins); copy the fields out first;
//! * don't nest `with_*_mut` calls for the same entity.
//!
//! Engine callbacks that pass an entity to modify (`HUD_AddEntity`, the studio and temp-entity
//! callbacks) and [`render::view_model`] hand out their own mutable views; the same rules
//! apply between those and the accessors here.

use crate::cl_eng;
use crate::client::ClEngine;
use crate::ext::render;
use crate::sys::*;
use crate::util::{cstr, to_cstring};
use std::ffi::{CStr, c_int};

/// The server command filter ([`set_server_cmd_filter`]): the line to send instead of `cmd`, or
/// `None` to send nothing.
pub type ServerCmdFilter = fn(cmd: &str) -> Option<String>;

static SERVER_CMD_FILTER: crate::Global<std::cell::Cell<Option<ServerCmdFilter>>> = crate::Global(std::cell::Cell::new(None));

/// Installs the filter [`ClEngine::server_cmd`] passes every line through. Port addition: the
/// client extensions' `server_cmd_pre` hook.
pub fn set_server_cmd_filter(f: Option<ServerCmdFilter>) {
    SERVER_CMD_FILTER.0.set(f);
}

/// `PM_STUDIO_BOX` trace flag for `EV_PlayerTrace` (SDK pm_defs.h: 0 normal, 1 studio ignore,
/// 2 studio box).
pub const PM_STUDIO_BOX: c_int = 2;

/// One entry of `SPR_GetList` (hud.txt), copied out of engine memory.
#[derive(Debug, Clone)]
pub struct SpriteEntry {
    pub name: String,
    pub sprite: String,
    pub res: i32,
    pub rc: wrect_t,
}

/// Screen size and font metrics from `pfnGetScreenInfo`.
#[derive(Clone)]
pub struct Screen {
    pub width: i32,
    pub height: i32,
    pub char_height: i32,
    pub char_widths: [i16; 256],
}

impl Default for Screen {
    fn default() -> Self {
        Screen { width: 640, height: 480, char_height: 12, char_widths: [8; 256] }
    }
}

/// Player info from `pfnGetPlayerInfo`.
#[derive(Debug, Clone, Default)]
pub struct PlayerInfo {
    /// The name decoded as UTF-8 (invalid bytes become U+FFFD).
    pub name: String,
    /// The name's raw bytes as the engine holds them (empty for a null name): what TS draws and
    /// prints.
    pub name_bytes: Vec<u8>,
    pub ping: i32,
    pub packetloss: i32,
    pub this_player: bool,
    pub spectator: bool,
    pub model: String,
    pub topcolor: i32,
    pub bottomcolor: i32,
}

impl ClEngine {
    /// `pfnSPR_Load`: the single wrapper for it. Guarded on Steam GoldSrc, whose `SPR_Load`
    /// `Sys_Error`s on a new name once its 256 HUD sprite slots are taken: such a load returns 0,
    /// FWGS's answer when its slots run out ([`crate::cl_guard::HudSprites`]).
    pub fn spr_load(&self, name: &str) -> HSPRITE {
        static SLOTS: std::sync::Mutex<Option<crate::cl_guard::HudSprites>> = std::sync::Mutex::new(None);
        let steam = !self.is_xash();
        if steam {
            let ok = SLOTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).may_load(name.as_bytes());
            if !ok {
                self.port_guard(&format!("SPR_Load {name}"), || format!("SPR_Load \"{name}\": no HUD sprite slot left; 0"));
                return 0;
            }
        }
        let s = to_cstring(name);
        let h = cl_eng!(pfnSPR_Load(s.as_ptr()));
        if steam {
            SLOTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).record(name.as_bytes(), h);
        }
        h
    }
    pub fn spr_width(&self, h: HSPRITE, frame: i32) -> i32 {
        if h == 0 { 0 } else { cl_eng!(pfnSPR_Width(h, frame)) }
    }
    pub fn spr_height(&self, h: HSPRITE, frame: i32) -> i32 {
        if h == 0 { 0 } else { cl_eng!(pfnSPR_Height(h, frame)) }
    }
    pub fn spr_set(&self, h: HSPRITE, rgb: [i32; 3]) {
        super::hud_record::set_sprite(h);
        cl_eng!(pfnSPR_Set(h, rgb[0], rgb[1], rgb[2]));
    }
    fn rc_ptr(rc: Option<&wrect_t>) -> *const wrect_t {
        rc.map_or(std::ptr::null(), |r| r as *const wrect_t)
    }
    /// A sprite draw's place for [`super::hud_record`]: false in a dry run.
    fn note_sprite(&self, frame: i32, x: i32, y: i32, rc: Option<&wrect_t>) -> bool {
        if !super::hud_record::recording() {
            return true;
        }
        let (w, h) = match rc {
            Some(r) => (r.right - r.left, r.bottom - r.top),
            None => {
                let spr = super::hud_record::sprite();
                (self.spr_width(spr, frame), self.spr_height(spr, frame))
            }
        };
        super::hud_record::note(x, y, w, h)
    }
    pub fn spr_draw(&self, frame: i32, x: i32, y: i32, rc: Option<&wrect_t>) {
        if self.note_sprite(frame, x, y, rc) {
            cl_eng!(pfnSPR_Draw(frame, x, y, Self::rc_ptr(rc)));
        }
    }
    pub fn spr_draw_additive(&self, frame: i32, x: i32, y: i32, rc: Option<&wrect_t>) {
        if self.note_sprite(frame, x, y, rc) {
            cl_eng!(pfnSPR_DrawAdditive(frame, x, y, Self::rc_ptr(rc)));
        }
    }
    pub fn spr_draw_holes(&self, frame: i32, x: i32, y: i32, rc: Option<&wrect_t>) {
        if self.note_sprite(frame, x, y, rc) {
            cl_eng!(pfnSPR_DrawHoles(frame, x, y, Self::rc_ptr(rc)));
        }
    }

    /// `SPR_GetList`: parses e.g. `sprites/hud.txt`. Entries are copied; the engine keeps the list.
    pub fn spr_get_list(&self, file: &str) -> Vec<SpriteEntry> {
        let s = to_cstring(file);
        let mut n: c_int = 0;
        let p = cl_eng!(pfnSPR_GetList(s.as_ptr().cast_mut(), &mut n));
        if p.is_null() || n <= 0 {
            return Vec::new();
        }
        // SAFETY: the engine returned `n` (> 0) sprite entries at non-null `p`, valid until the next `SPR_GetList`; copied below.
        let list = unsafe { std::slice::from_raw_parts(p, n as usize) };
        list.iter()
            .map(|e| SpriteEntry {
                // SAFETY: the sprite list entries hold NUL-terminated names (engine-filled char arrays).
                name: unsafe { cstr(e.szName.as_ptr()) }.to_string_lossy().into_owned(),
                // SAFETY: the sprite list entries hold NUL-terminated names (engine-filled char arrays).
                sprite: unsafe { cstr(e.szSprite.as_ptr()) }.to_string_lossy().into_owned(),
                res: e.iRes,
                rc: e.rc,
            })
            .collect()
    }

    /// `pfnGetScreenInfo`: the single wrapper for it.
    pub fn screen(&self) -> Screen {
        let mut info = SCREENINFO { iSize: size_of::<SCREENINFO>() as i32, ..Default::default() };
        cl_eng!(pfnGetScreenInfo(&mut info));
        super::hud_record::set_char_height(info.iCharHeight);
        Screen { width: info.iWidth, height: info.iHeight, char_height: info.iCharHeight, char_widths: info.charWidths }
    }

    /// `pfnDrawCharacter`: returns the advance.
    pub fn draw_character(&self, x: i32, y: i32, ch: u8, rgb: [i32; 3]) -> i32 {
        if super::hud_record::recording() {
            // The advance, from the screen's character widths, without drawing.
            let w = self.screen().char_widths.get(ch as usize).map_or(0, |&w| w as i32);
            if !super::hud_record::note(x, y, w, super::hud_record::char_height()) {
                return w;
            }
        }
        cl_eng!(pfnDrawCharacter(x, y, ch as c_int, rgb[0], rgb[1], rgb[2]))
    }

    /// `pfnDrawSetTextColor` + `pfnDrawConsoleString`: the single wrapper for them. Console font
    /// string with an explicit colour; returns the x after the text.
    pub fn draw_console_string(&self, x: i32, y: i32, rgb: [f32; 3], text: &str) -> i32 {
        if super::hud_record::recording() {
            let (w, h) = self.console_string_size(text);
            if !super::hud_record::note(x, y, w, h) {
                return x + w;
            }
        }
        let s = to_cstring(text);
        cl_eng!(pfnDrawSetTextColor(rgb[0], rgb[1], rgb[2]));
        cl_eng!(pfnDrawConsoleString(x, y, s.as_ptr().cast_mut()))
    }

    pub fn console_string_size(&self, text: &str) -> (i32, i32) {
        let s = to_cstring(text);
        let (mut w, mut h) = (0, 0);
        cl_eng!(pfnDrawConsoleStringLen(s.as_ptr(), &mut w, &mut h));
        (w, h)
    }

    /// `pfnFillRGBA`: the single wrapper for it. The engine draws it additively.
    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, rgb: [i32; 3], a: i32) {
        if super::hud_record::note(x, y, w, h) {
            cl_eng!(pfnFillRGBA(x, y, w, h, rgb[0], rgb[1], rgb[2], a));
        }
    }

    /// An alpha-blended fill, as the engine's VGUI surface draws panels and text: the single
    /// `pfnFillRGBABlend` wrapper. That slot came after the 2007 table (SteamPipe, 2013), so it is
    /// only used on engines known to have it: FWGS (its `host_ver` cvar) and Steam GoldSrc builds
    /// of that update or later (its `m_rawinput` cvar). Elsewhere this falls back to the additive
    /// [`fill`](Self::fill).
    pub fn fill_blend(&self, x: i32, y: i32, w: i32, h: i32, rgb: [i32; 3], a: i32) {
        static HAS_BLEND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let has = *HAS_BLEND.get_or_init(|| {
            let found = |n: &'static CStr| !crate::client::Cvar::find(n).is_null();
            self.f.pfnFillRGBABlend.is_some() && (found(c"host_ver") || found(c"m_rawinput"))
        });
        if has {
            if super::hud_record::note(x, y, w, h) {
                cl_eng!(pfnFillRGBABlend(x, y, w, h, rgb[0], rgb[1], rgb[2], a));
            }
        } else {
            self.fill(x, y, w, h, rgb, a);
        }
    }

    /// `pfnDrawConsoleString` on raw bytes (no UTF-8 conversion), with `rgb` set first through
    /// `pfnDrawSetTextColor`, or in the engine's default text colour when `None` (the engine
    /// resets the colour after every string). Returns the x after the text.
    pub fn draw_console_bytes(&self, x: i32, y: i32, rgb: Option<[f32; 3]>, text: &[u8]) -> i32 {
        if super::hud_record::recording() {
            let (w, h) = self.console_bytes_size(text);
            if !super::hud_record::note(x, y, w, h) {
                return x + w;
            }
        }
        let s = std::ffi::CString::new(text.iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>()).unwrap_or_default();
        if let Some(c) = rgb {
            cl_eng!(pfnDrawSetTextColor(c[0], c[1], c[2]));
        }
        cl_eng!(pfnDrawConsoleString(x, y, s.as_ptr().cast_mut()))
    }

    /// `pfnDrawConsoleStringLen` on raw bytes: (width, height).
    pub fn console_bytes_size(&self, text: &[u8]) -> (i32, i32) {
        let s = std::ffi::CString::new(text.iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>()).unwrap_or_default();
        let (mut w, mut h) = (0, 0);
        cl_eng!(pfnDrawConsoleStringLen(s.as_ptr(), &mut w, &mut h));
        (w, h)
    }

    /// `pfnConsolePrint` on raw bytes.
    pub fn print_bytes(&self, text: &[u8]) {
        let s = std::ffi::CString::new(text.iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>()).unwrap_or_default();
        cl_eng!(pfnConsolePrint(s.as_ptr()))
    }

    /// `pfnServerCmd`: the single wrapper for it. Every engine fills the slot; if it is null the
    /// command is dropped rather than panicking, since callers are fire-and-forget.
    ///
    /// The server command filter ([`set_server_cmd_filter`]) sees the line first and may block or
    /// rewrite it.
    pub fn server_cmd(&self, cmd: &str) {
        match SERVER_CMD_FILTER.0.get() {
            Some(filter) => {
                if let Some(line) = filter(cmd) {
                    self.server_cmd_unfiltered(&line);
                }
            }
            None => self.server_cmd_unfiltered(cmd),
        }
    }
    /// `pfnServerCmd` without the filter (the client extensions' own commands).
    pub fn server_cmd_unfiltered(&self, cmd: &str) {
        let Some(f) = self.f.pfnServerCmd else { return };
        let s = to_cstring(cmd);
        // SAFETY: engine callback from the client function table, called with a NUL-terminated string that outlives the call.
        unsafe { f(s.as_ptr()) };
    }
    pub fn client_cmd(&self, cmd: &str) {
        let s = to_cstring(cmd);
        cl_eng!(pfnClientCmd(s.as_ptr()));
    }
    /// `Con_IsVisible`: the console is down.
    pub fn con_is_visible(&self) -> bool {
        cl_eng!(Con_IsVisible()) != 0
    }
    pub fn center_print(&self, text: &str) {
        let s = to_cstring(text);
        cl_eng!(pfnCenterPrint(s.as_ptr()));
    }
    /// `pfnCenterPrint` on raw bytes (no UTF-8 conversion).
    pub fn center_print_bytes(&self, text: &[u8]) {
        // The engine draws it later, somewhere it doesn't say: a dry run prints nothing.
        if !super::hud_record::may_draw() {
            return;
        }
        let s = std::ffi::CString::new(text.iter().copied().filter(|&b| b != 0).collect::<Vec<u8>>()).unwrap_or_default();
        cl_eng!(pfnCenterPrint(s.as_ptr()));
    }

    pub fn cvar_string(&self, name: &CStr) -> String {
        let p = cl_eng!(pfnGetCvarString(name.as_ptr()));
        // SAFETY: the engine returns null or a NUL-terminated string it owns; it's used or copied before the next engine call.
        unsafe { cstr(p) }.to_string_lossy().into_owned()
    }
    pub fn cvar_set(&self, name: &CStr, value: f32) {
        cl_eng!(Cvar_SetValue(name.as_ptr(), value));
    }
    pub fn cmd_argc(&self) -> i32 {
        cl_eng!(Cmd_Argc())
    }

    /// `Key_LookupBinding`: the name of a key bound to `binding` (the command without its `+`;
    /// the engine skips a bound command's leading `+` when it compares), or `None` when no key
    /// is bound. The engine's string is copied out at once. A `binding` with a NUL byte can't be
    /// passed to C and reads as unbound.
    pub fn key_lookup_binding(&self, binding: &[u8]) -> Option<Vec<u8>> {
        let s = std::ffi::CString::new(binding).ok()?;
        let p = cl_eng!(Key_LookupBinding(s.as_ptr()));
        // SAFETY: the engine returns null or a NUL-terminated key name it owns; it's copied here,
        // before any other engine call.
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_bytes().to_vec())
    }

    /// `pfnTextMessageGet` (titles.txt): the message, if the key exists.
    pub fn text_message(&self, name: &str) -> Option<TextMessage> {
        let s = to_cstring(name);
        let p = cl_eng!(pfnTextMessageGet(s.as_ptr()));
        // SAFETY: the engine's titles.txt entries live for the process.
        if p.is_null() { None } else { Some(TextMessage(unsafe { &*p })) }
    }

    pub fn player_info(&self, index: i32) -> PlayerInfo {
        let mut info = hud_player_info_t::default();
        cl_eng!(pfnGetPlayerInfo(index, &mut info));
        // SAFETY: the engine returns null or a NUL-terminated string it owns; it's used or copied before the next engine call.
        let name = unsafe { cstr(info.name) };
        PlayerInfo {
            name: name.to_string_lossy().into_owned(),
            name_bytes: name.to_bytes().to_vec(),
            ping: info.ping as i32,
            packetloss: info.packetloss as i32,
            this_player: info.thisplayer != 0,
            spectator: info.spectator != 0,
            // SAFETY: the engine returns null or a NUL-terminated string it owns; it's used or copied before the next engine call.
            model: unsafe { cstr(info.model) }.to_string_lossy().into_owned(),
            topcolor: info.topcolor as i32,
            bottomcolor: info.bottomcolor as i32,
        }
    }

    /// `GetLocalPlayer`: the single wrapper for it (the accessors below go through it).
    fn local_player_ptr(&self) -> *mut cl_entity_t {
        cl_eng!(GetLocalPlayer())
    }
    /// `GetEntityByIndex`: the single wrapper for it (the accessors below go through it).
    fn entity_ptr(&self, index: i32) -> *mut cl_entity_t {
        cl_eng!(GetEntityByIndex(index))
    }
    /// The local player's entity: a shared, read-only view. See the module docs.
    pub fn local_player(&self) -> Option<&'static cl_entity_t> {
        // SAFETY: null or an entry of the engine's entity array, which lives for the process;
        // read-only and short-lived per the module contract.
        unsafe { self.local_player_ptr().as_ref() }
    }
    /// Entity `index`: a shared, read-only view. See the module docs.
    pub fn entity(&self, index: i32) -> Option<&'static cl_entity_t> {
        // SAFETY: as in `local_player`.
        unsafe { self.entity_ptr(index).as_ref() }
    }
    /// Runs `f` on the local player's entity with a mutable view scoped to the call (the SDK's
    /// `V_CalcRefdef` edits it: render mode, model origin/angles). `None` if there is none. No
    /// reference from [`local_player`](Self::local_player)/[`entity`](Self::entity) may be
    /// held across the call.
    pub fn with_local_player_mut<R>(&self, f: impl FnOnce(&mut cl_entity_t) -> R) -> Option<R> {
        // SAFETY: as in `local_player`; the exclusive borrow ends with `f`, and the module
        // contract forbids holding another view of the entry across it.
        unsafe { self.local_player_ptr().as_mut() }.map(f)
    }
    /// Runs `f` on entity `index` with a mutable view scoped to the call. `None` if there is
    /// none. Same rule as [`with_local_player_mut`](Self::with_local_player_mut).
    pub fn with_entity_mut<R>(&self, index: i32, f: impl FnOnce(&mut cl_entity_t) -> R) -> Option<R> {
        // SAFETY: as in `with_local_player_mut`.
        unsafe { self.entity_ptr(index).as_mut() }.map(f)
    }
    /// `GetMaxClients`: the single wrapper for it.
    pub fn max_clients(&self) -> i32 {
        cl_eng!(GetMaxClients())
    }
    /// `GetClientTime`: the single wrapper for it.
    pub fn client_time(&self) -> f32 {
        cl_eng!(GetClientTime())
    }
    pub fn mouse_position(&self) -> (i32, i32) {
        let (mut x, mut y) = (0, 0);
        cl_eng!(GetMousePosition(&mut x, &mut y));
        (x, y)
    }
    pub fn set_view_angles(&self, mut a: [f32; 3]) {
        cl_eng!(SetViewAngles(a.as_mut_ptr()));
    }
    pub fn key_event(&self, key: i32, down: bool) {
        cl_eng!(Key_Event(key, down as c_int));
    }
    /// `pfnPlaySoundByName`. Guarded: a name too long for the engine is dropped.
    pub fn play_sound(&self, name: &str, volume: f32) {
        if !self.sound_name_ok("PlaySoundByName", name) {
            return;
        }
        let s = to_cstring(name);
        cl_eng!(pfnPlaySoundByName(s.as_ptr().cast_mut(), volume));
    }
    /// `pfnPlaySoundByNameAtLocation`: a sound at a point, no entity or channel, normal pitch.
    /// Guarded like [`Self::play_sound`].
    pub fn play_sound_at(&self, name: &str, volume: f32, origin: [f32; 3]) {
        if !self.sound_name_ok("PlaySoundByNameAtLocation", name) {
            return;
        }
        let s = to_cstring(name);
        let mut o = origin;
        cl_eng!(pfnPlaySoundByNameAtLocation(s.as_ptr().cast_mut(), volume, o.as_mut_ptr()));
    }

    /// Steam GoldSrc's `S_FindName` `Sys_Error`s on a sound name over 63 bytes ("Sound name too
    /// long"); FWGS refuses the same names without a word, so they are dropped on every engine.
    fn sound_name_ok(&self, call: &str, name: &str) -> bool {
        let ok = name.bytes().filter(|&b| b != 0).count() <= crate::cl_guard::MAX_SOUND_NAME;
        if !ok {
            self.port_guard(&format!("{call} {name}"), || format!("{call} \"{name}\": sound name too long; skipped"));
        }
        ok
    }
    pub fn game_directory(&self) -> String {
        let p = cl_eng!(pfnGetGameDirectory());
        // SAFETY: the engine returns null or a NUL-terminated string it owns; it's used or copied before the next engine call.
        unsafe { cstr(p) }.to_string_lossy().into_owned()
    }

    /// `EV_LocalPlayerViewheight`: the local player's view offset. The single wrapper for it,
    /// used by the crosshair ID and by the events' `EV_GetGunPosition`. Every engine fills the
    /// slot; if the table or slot is null this returns the standing view offset (0, 0, 28)
    /// instead of panicking, since both callers only need a plausible eye position.
    pub fn local_view_height(&self) -> [f32; 3] {
        let mut v = [0.0, 0.0, 28.0];
        if let Some(f) = self.event_api().and_then(|a| a.EV_LocalPlayerViewheight) {
            // SAFETY: engine callback from the event API table, called with a valid 3-float buffer.
            unsafe { f(v.as_mut_ptr()) };
        }
        v
    }

    /// The event API (`EV_PlayerTrace` etc.), if the engine provides one. Only slots of the 2007
    /// SDK `event_api_s` are called; that table is a prefix of the FWGS one bindgen generates.
    pub fn event_api(&self) -> Option<&'static event_api_s> {
        // SAFETY: the engine sets `pEventAPI` (or leaves it null) before `Initialize` and keeps the table for the process.
        unsafe { self.f.pEventAPI.as_ref() }
    }
    /// The net API's `Status`: whether the client is connected, and the server's address as the
    /// engine's `AdrToString` writes it (`203.0.113.5:27015`, `loopback` on a listen server).
    /// `None` when the engine has no net API or leaves either slot null. Port addition for
    /// restart and reconnect (docs/implementation/client-plugins.md §13.5).
    pub fn net_status(&self) -> Option<(bool, String)> {
        // SAFETY: the engine sets `pNetAPI` (or leaves it null) before `Initialize` and keeps the table for the process.
        let api = unsafe { self.f.pNetAPI.as_ref() }?;
        let (status_fn, adr_fn) = (api.Status?, api.AdrToString?);
        let mut st = net_status_t::default();
        // SAFETY: `Status` fills the `net_status_t` it is given; `st` is a local of that type.
        unsafe { status_fn(&mut st) };
        // SAFETY: `AdrToString` reads the address it is given and returns null or a NUL-terminated
        // string in an engine buffer, copied before the next engine call.
        let adr = unsafe { cstr(adr_fn(&mut st.remote_address)) }.to_string_lossy().into_owned();
        Some((st.connected != 0, adr))
    }
    /// The demo API (`IsPlayingback` etc.), if the engine provides one.
    pub fn demo_api(&self) -> Option<&'static demo_api_s> {
        // SAFETY: the engine sets `pDemoAPI` (or leaves it null) before `Initialize` and keeps the table for the process.
        unsafe { self.f.pDemoAPI.as_ref() }
    }
    /// `IsPlayingback`; false if the table or slot is null.
    pub fn demo_playing(&self) -> bool {
        // SAFETY: engine callback from the demo API table, no arguments.
        self.demo_api().and_then(|d| d.IsPlayingback).is_some_and(|f| unsafe { f() } != 0)
    }
    /// `IsRecording`; false if the table or slot is null.
    pub fn demo_recording(&self) -> bool {
        // SAFETY: engine callback from the demo API table, no arguments.
        self.demo_api().and_then(|d| d.IsRecording).is_some_and(|f| unsafe { f() } != 0)
    }
    /// `WriteBuffer`: appends client data to the demo being recorded (the engine hands it back to
    /// `Demo_ReadBuffer` on playback). No-op if the table or slot is null.
    pub fn demo_write_buffer(&self, buf: &[u8]) {
        if let Some(f) = self.demo_api().and_then(|d| d.WriteBuffer) {
            let mut copy = buf.to_vec();
            // SAFETY: engine callback from the demo API table; it reads `size` bytes from the
            // buffer, which is a live local copy for the call.
            unsafe { f(copy.len() as c_int, copy.as_mut_ptr()) };
        }
    }

    /// The effects API (`R_TempModel`, `CL_AllocDlight` etc.), if the engine provides one. Only
    /// slots of the 2007 SDK `efx_api_s` are called; that table is a prefix of the FWGS one
    /// bindgen generates.
    pub fn efx_api(&self) -> Option<&'static efx_api_s> {
        // SAFETY: the engine sets `pEfxAPI` (or leaves it null) before `Initialize` and keeps the table for the process.
        unsafe { self.f.pEfxAPI.as_ref() }
    }

    /// `EV_PlayerTrace` from `start` to `end` against the world and the other players (studio
    /// boxes, hull 2), as TS's crosshair ID does (`DrawCrosshairID` 1004acc0). Returns
    /// (fraction, endpos, `EV_IndexFromTrace`), or `None` if the engine has no event API.
    /// Built on [`render::with_solid_players`] and [`render::player_trace`], the single
    /// wrappers for those calls.
    ///
    /// As TS, `ignore_pe` is the local player's entity index and `EV_IndexFromTrace` is called
    /// whether or not the trace hit. `ignore_pe` is an index into the physent list (world, then
    /// the frame's solid non-player entities, then the other players), so this skips whatever
    /// sits at that slot, not the local player, who `EV_SetSolidPlayers` already leaves out. A
    /// miss leaves `tr.ent` at -1, for which FWGS returns -1 (no entity). See
    /// docs/implementation/client-hud.md §5.
    pub fn crosshair_trace(&self, start: [f32; 3], end: [f32; 3], local_index: i32) -> Option<(f32, [f32; 3], i32)> {
        self.event_api()?;
        Some(render::with_solid_players(local_index.wrapping_sub(1), || {
            let mut tr = render::player_trace(start, end, 2, PM_STUDIO_BOX, local_index);
            let ent = render::index_from_trace(&mut tr);
            (tr.fraction, tr.endpos, ent)
        }))
    }
}

impl ClEngine {
    /// The triangle API (`WorldToScreen` etc.), if the engine provides one. Only slots of the
    /// 2007 SDK `triangleapi_s` are called; that table is a prefix of the FWGS one bindgen
    /// generates.
    pub fn tri_api(&self) -> Option<&'static triangleapi_s> {
        // SAFETY: the engine sets `pTriAPI` (or leaves it null) before `Initialize` and keeps the table for the process.
        unsafe { self.f.pTriAPI.as_ref() }
    }

    /// Triangle API `WorldToScreen`: the single wrapper for it. Returns the normalised device
    /// coordinates (-1..1, y up) of a world point, or `None` when it is z-clipped (behind the
    /// view) or the engine left the table or slot null. Not bounds-checked: the laser dots
    /// (`DrawLaserDots`) are converted to pixels without a screen check.
    pub fn world_to_screen_ndc(&self, world: [f32; 3]) -> Option<[f32; 2]> {
        let (out, clipped) = self.world_to_screen_raw(world)?;
        (!clipped).then_some([out[0], out[1]])
    }

    /// Triangle API `WorldToScreen` for callers that use the output whatever the result (TS's
    /// overview name tags): what the engine wrote (the normalised device coordinates, or, when
    /// the point is z-clipped, the undivided clip coordinates FWGS's `R_WorldToScreen` leaves) and
    /// whether it was clipped. `None` when the engine left the table or slot null.
    pub fn world_to_screen_raw(&self, world: [f32; 3]) -> Option<([f32; 3], bool)> {
        let f = self.tri_api()?.WorldToScreen?;
        let mut out = [0.0f32; 3];
        // SAFETY: engine callback from the triangle API table, called with a 3-float input and a 3-float output buffer.
        let clipped = unsafe { f(world.as_ptr(), out.as_mut_ptr()) };
        Some((out, clipped != 0))
    }

    /// TS `WorldToScreen` (100579c0): screen pixel position of a world point,
    /// `ftol((1 + nx) · w · 0.5)` and `ftol((1 − ny) · h · 0.5)` on the x87, or `None` when it is
    /// behind the view (TS's −1, −1). Not bounds-checked, as in TS: a point just off screen still
    /// gives a position, so a marker centred there shows its visible part.
    pub fn world_to_screen(&self, world: [f32; 3], screen: &Screen) -> Option<(i32, i32)> {
        let [nx, ny] = self.world_to_screen_ndc(world)?;
        // MSVC `__ftol`: truncation, the integer indefinite value out of range.
        let ftol = |v: f64| if (-2147483648.0..2147483648.0).contains(&v) { v.trunc() as i32 } else { i32::MIN };
        let x = ftol((nx as f64 + 1.0) * screen.width as f64 * 0.5);
        let y = ftol((1.0 - ny as f64) * screen.height as f64 * 0.5);
        Some((x, y))
    }
}

/// `SND_CHANGE_VOL` for [`ClEngine::ev_play_sound`].
pub const SND_CHANGE_VOL: i32 = 1 << 6;

impl ClEngine {
    /// `EV_PlaySound(ent, origin, channel, sample, volume, attenuation, flags, pitch)`: the
    /// single wrapper for it. `ent` 0 plays at `origin`, -1 is a positional sound with no
    /// entity. Every engine fills the slot; if the table or slot is null the sound is dropped
    /// rather than panicking, since callers are fire-and-forget. A name too long for the engine
    /// is dropped too (see `play_sound`).
    #[allow(clippy::too_many_arguments)]
    pub fn ev_play_sound(&self, ent: i32, origin: [f32; 3], channel: i32, sample: &str, volume: f32, attn: f32, flags: i32, pitch: i32) {
        let Some(f) = self.event_api().and_then(|a| a.EV_PlaySound) else { return };
        if !self.sound_name_ok("EV_PlaySound", sample) {
            return;
        }
        let s = to_cstring(sample);
        let mut o = origin;
        // SAFETY: engine callback from the event API table, called with a 3-float origin and a NUL-terminated sample that outlive the call.
        unsafe { f(ent, o.as_mut_ptr(), channel, s.as_ptr(), volume, attn, flags, pitch) };
    }
}

/// A `titles.txt` entry from `ClEngine::text_message`. Derefs to the engine struct; the text
/// is read through [`TextMessage::message`].
#[derive(Clone, Copy)]
pub struct TextMessage(&'static client_textmessage_t);

impl std::ops::Deref for TextMessage {
    type Target = client_textmessage_t;
    fn deref(&self) -> &client_textmessage_t {
        self.0
    }
}

impl TextMessage {
    /// The message text (empty if the engine left it null).
    pub fn message(&self) -> &'static CStr {
        // SAFETY: the engine fills pMessage with a NUL-terminated string that lives with the entry.
        unsafe { crate::util::cstr(self.0.pMessage) }
    }
}

/// `net_status_t` (SDK and FWGS `net_api.h`), mirrored because the bindings leave the net API
/// opaque. Both engines' `netadr_t` is 20 bytes with the IPv4 fields where Quake 2 had them
/// (FWGS packs its IPv6 variant around them, `netadr.h`), so the layout is the same; the tail is
/// padding in case an engine's struct is longer.
#[repr(C)]
struct NetStatus {
    connected: c_int,
    local_address: [u8; 20],
    remote_address: [u8; 20],
    packet_loss: c_int,
    latency: f64,
    connection_time: f64,
    rate: f64,
    _spare: [u8; 64],
}

/// The first slots of `net_api_t`.
#[repr(C)]
struct NetApi {
    init_networking: Option<unsafe extern "C" fn()>,
    status: Option<unsafe extern "C" fn(*mut NetStatus)>,
}

/// A `netadr_t` as text: `loopback`, `a.b.c.d:port`, or `None` for another kind (IPX, IPv6)
/// or none. `type` is the first u16 (FWGS's `type`; the low half of GoldSrc's 32-bit enum), the
/// IPv4 address bytes 4..8, the port (network order) bytes 18..20.
pub fn netadr_text(adr: &[u8; 20]) -> Option<String> {
    const NA_LOOPBACK: u16 = 1;
    const NA_IP: u16 = 3;
    match u16::from_le_bytes([adr[0], adr[1]]) {
        NA_LOOPBACK => Some("loopback".to_owned()),
        NA_IP => Some(format!("{}.{}.{}.{}:{}", adr[4], adr[5], adr[6], adr[7], u16::from_be_bytes([adr[18], adr[19]]))),
        _ => None,
    }
}

impl ClEngine {
    /// The server the client is connected to (net API `Status`): `loopback` for a listen server,
    /// `a.b.c.d:port`, or `None` while not connected (or on an engine without the net API).
    /// Port addition for client extensions (issue #293).
    pub fn server_address(&self) -> Option<String> {
        // SAFETY: the engine sets `pNetAPI` (or leaves it null) before `Initialize` and keeps the
        // table for the process; its first two slots are `InitNetworking` and `Status`.
        let api = unsafe { self.f.pNetAPI.cast::<NetApi>().as_ref() }?;
        let status = api.status?;
        // SAFETY: plain data, all-zero is valid.
        let mut st: NetStatus = unsafe { std::mem::zeroed() };
        // SAFETY: engine callback from the net API table; it fills a `net_status_t`, which
        // `NetStatus` lays out (with spare room after it).
        unsafe { status(&mut st) };
        if st.connected == 0 {
            return None;
        }
        netadr_text(&st.remote_address)
    }
}

#[cfg(test)]
mod net_tests {
    use super::netadr_text;

    #[test]
    fn addresses() {
        let mut a = [0u8; 20];
        a[0] = 1;
        assert_eq!(netadr_text(&a).as_deref(), Some("loopback"));
        a[0] = 3;
        a[4..8].copy_from_slice(&[127, 0, 0, 1]);
        a[18..20].copy_from_slice(&27015u16.to_be_bytes());
        assert_eq!(netadr_text(&a).as_deref(), Some("127.0.0.1:27015"));
        a[0] = 6;
        assert_eq!(netadr_text(&a), None, "IPv6");
        assert_eq!(netadr_text(&[0; 20]), None);
    }
}
