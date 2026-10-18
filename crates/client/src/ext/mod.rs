//! Client extensions (port addition, issue #293; docs/implementation/client-ext-api.md): native
//! `cl_dlls/tsmod_<name>` libraries the client loads through the C ABI in `ts-client-ext-api`.
//! `ts-client-dll` does the FFI (opening a library, its `TSRS_ClientExtLoad`, the
//! `ClientExtApi` table, [`ExtHooks`] over its `ClientExt`); this module decides what loads
//! ([`load_all`], through the [`gate`]), keeps the loaded extensions, calls them at the right
//! points of the client's exports, and implements the table's functions safely
//! (`host_*`, [`view`]).
//!
//! # When the extensions run
//! | Callback | From |
//! |---|---|
//! | `init` | the end of `HUD_Init` ([`init`]) |
//! | `vid_init` | the end of `HUD_VidInit` |
//! | `frame` | the end of `HUD_Frame`, after the server-change check |
//! | `redraw` | `HUD_Redraw`, after the HUD and the menus, desktop GL state saved around it |
//! | `key_event` | the start of `HUD_Key_Event`; a swallowed key goes no further |
//! | `mouse_active` | `IN_ActivateMouse` / `IN_DeactivateMouse` |
//! | `wants_cursor` | once a frame, from `HUD_Frame` ([`cursor_wanted`] reads the answer) |
//! | `usercmd` | the end of `CL_CreateMove` |
//! | `game_event` | after the HUD handled a message ([`on_hud_message`]) |
//! | `server_changed` | `HUD_Frame`, when the net status' server changes |
//! | `shutdown` | `HUD_Shutdown` (even while suspended) |
//! | `user_message` | a `TSExt` message on a channel the extension claimed ([`on_tsext`]) |
//! | `message_pre` | each user message, before the HUD ([`message_pre`], [`intercept`]) |
//! | `command_pre` | each port client command, before it runs (`goldsrc`'s command filter) |
//! | `server_cmd_pre` | each command the port sends to the server (`goldsrc`'s server command filter) |
//! | `menu_shown` | once a frame, for each menu the extension took over ([`takeover`]) |
//! | `menu_closed` | a takeover ended without the extension's answer |
//!
//! Every dispatch copies the list of extensions out first, so an extension may call the table
//! (which reads this module's state) from any callback.

pub mod alive;
pub mod events;
pub mod gate;
pub mod intercept;
pub mod takeover;
pub mod view;

use gate::{ApprovalGate, Approved, Candidate};
use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
pub use intercept::{Dispatch, Verdict};
use shared::plugin_lib;
use shared::protocol::Msg;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ts_client_ext_api::{EXISTS, ExtId, FAILED, GameEvent, INVALID, NOT_FOUND, OK, REFUSED, SUSPENDED, UserCmd, VisiblePlayer, allow};

/// A user message for [`ExtHooks::message_pre`].
pub struct MsgCall<'a> {
    pub name: &'a CStr,
    pub data: &'a [u8],
    pub allow: u32,
    pub token: u32,
}

/// A command for [`ExtHooks::command_pre`] and [`ExtHooks::server_cmd_pre`].
pub struct CmdCall<'a> {
    pub line: &'a CStr,
    pub argv: &'a [CString],
    pub allow: u32,
}

/// A loaded extension's callbacks. `ts-client-dll` implements it over the extension's C
/// descriptor; every method does nothing unless the extension gave that callback.
pub trait ExtHooks {
    /// A `TSExt` message on one of its channels.
    fn user_message(&self, _channel: &CStr, _data: &[u8]) {}
    /// Its verdict on a user message ([`intercept`]).
    fn message_pre(&self, _msg: &MsgCall) -> Verdict {
        Verdict::Pass
    }
    /// Its verdict on a client command (`Pass` or `Block`).
    fn command_pre(&self, _cmd: &CmdCall) -> Verdict {
        Verdict::Pass
    }
    /// Its verdict on a command to the server (`Replace` holds the new line).
    fn server_cmd_pre(&self, _cmd: &CmdCall) -> Verdict {
        Verdict::Pass
    }
    fn init(&self) {}
    fn vid_init(&self) {}
    fn frame(&self, _time: f64) {}
    fn redraw(&self, _time: f32, _intermission: bool) {}
    /// True if the extension swallowed the key.
    fn key_event(&self, _down: bool, _key: i32, _binding: &CStr) -> bool {
        false
    }
    fn mouse_active(&self, _active: bool) {}
    fn wants_cursor(&self) -> bool {
        false
    }
    fn usercmd(&self, _cmd: &UserCmd) {}
    fn game_event(&self, _event: &GameEvent) {}
    fn suspend(&self, _reason: &CStr) {}
    fn resume(&self) {}
    fn server_changed(&self, _address: &CStr) {}
    fn shutdown(&self) {}
    /// Whether it still shows the menu of `kind` it took over; `None` without the callback.
    fn menu_shown(&self, _kind: i32) -> Option<bool> {
        None
    }
    fn menu_closed(&self, _kind: i32, _reason: i32) {}
}

/// Opens one approved library as extension `id` (the FFI in `ts-client-dll`): its callbacks and
/// a description for the console, or why it didn't load.
pub type Open = &'static dyn Fn(&Candidate, ExtId) -> Result<(Rc<dyn ExtHooks>, String), String>;

struct Entry {
    id: ExtId,
    name: String,
    library: PathBuf,
    approved: Approved,
    /// `None` while its load function runs.
    hooks: Option<Rc<dyn ExtHooks>>,
    suspended: bool,
    /// Why it is suspended (`cl_plugins_list`, client-plugins.md §13.4).
    reason: String,
    /// The HUD elements it hides (`ClientExtApi::hud_set_hidden`, issue #314).
    hud_hidden: u64,
}

struct Command {
    /// Lower case: the engines match command names case-insensitively.
    name: String,
    ext: ExtId,
    f: ts_client_ext_api::CommandFn,
}

#[derive(Default)]
struct State {
    exts: Vec<Entry>,
    /// `None` until [`set_gate`] or the first load; taken out while a gate method runs.
    gate: Option<Box<dyn ApprovalGate>>,
    next_id: ExtId,
    commands: Vec<Command>,
    game_dir: Option<PathBuf>,
    /// `HUD_Init` / a `HUD_VidInit` ran (an extension loaded late catches up, client-plugins.md §9).
    hud_init: bool,
    vid_init: bool,
    frame: u64,
    prev_buttons: u32,
    snapshot: events::Snapshot,
    /// The server the last frame saw (`None` not connected).
    server: Option<String>,
    cursor: bool,
    visible: Option<(u64, Vec<VisiblePlayer>)>,
    /// What [`load_all`] opened libraries with, for the plugins approved for one server only
    /// (loaded on connecting there, client-plugins.md §9).
    open: Option<Open>,
    /// The gate has candidates for some server ([`ApprovalGate::waits_for_server`]).
    server_wait: bool,
    /// `TSExt` channels and the extension that claimed each.
    channels: Vec<(Vec<u8>, ExtId)>,
    /// Held messages ([`intercept::Holds`]); taken out while extensions are asked.
    holds: intercept::Holds,
    /// Interception warnings already printed (once per extension and text).
    warned: Vec<(ExtId, String)>,
    /// Other players' deaths, for `visible_players` (issue #306).
    deaths: alive::Deaths,
    /// The menus extensions took over (issue #314).
    takeovers: takeover::Takeovers,
}

static STATE: Global<RefCell<Option<State>>> = Global(RefCell::new(None));

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut s = STATE.0.borrow_mut();
    f(s.get_or_insert_with(|| State { next_id: 1, ..Default::default() }))
}

/// The loaded, not suspended extensions' callbacks, in load order.
fn active() -> Vec<Rc<dyn ExtHooks>> {
    with(|s| s.exts.iter().filter(|e| !e.suspended).filter_map(|e| e.hooks.clone()).collect())
}

/// Every loaded extension's callbacks, suspended or not.
fn all() -> Vec<Rc<dyn ExtHooks>> {
    with(|s| s.exts.iter().filter_map(|e| e.hooks.clone()).collect())
}

/// The loaded, not suspended extensions with their ids, in load order.
fn active_ids() -> Vec<(ExtId, Rc<dyn ExtHooks>)> {
    with(|s| s.exts.iter().filter(|e| !e.suspended).filter_map(|e| Some((e.id, e.hooks.clone()?))).collect())
}

/// Extension `id`'s callbacks while it is loaded and not suspended.
fn active_hooks(id: ExtId) -> Option<Rc<dyn ExtHooks>> {
    with(|s| s.exts.iter().find(|e| e.id == id && !e.suspended).and_then(|e| e.hooks.clone()))
}

fn say(line: &str) {
    engine().print(&format!("[ts] client extensions: {line}\n"));
}

/// Installs the approval gate (issue #297's approval store). Call before [`load_all`], at
/// `Initialize`; it replaces the default gate ([`gate::default_gate`]).
pub fn set_gate(g: Box<dyn ApprovalGate>) {
    with(|s| s.gate = Some(g));
}

/// Runs `f` with the gate taken out of the state (so a gate may print or look things up), then
/// puts it back.
fn with_gate<R>(f: impl FnOnce(&dyn ApprovalGate) -> R) -> R {
    let g = with(|s| s.gate.take()).unwrap_or_else(|| {
        let (g, note) = gate::default_gate(std::env::var(gate::DEV_ENV).ok().as_deref());
        if let Some(n) = note {
            say(&n);
        }
        g
    });
    let r = f(&*g);
    with(|s| s.gate = Some(g));
    r
}

/// The game directory as an absolute path (`<cwd>/<pfnGetGameDirectory>`), worked out once.
pub fn game_dir() -> PathBuf {
    if let Some(d) = with(|s| s.game_dir.clone()) {
        return d;
    }
    let rel = PathBuf::from(engine().game_directory());
    let d = std::path::absolute(&rel).unwrap_or(rel);
    with(|s| s.game_dir = Some(d.clone()));
    d
}

/// `Initialize`: every approved extension in `<gamedir>/cl_dlls` (and the gate's packaged ones),
/// once per process. `open` maps each one (`ts-client-dll`).
pub fn load_all(open: Open) {
    static DONE: Global<std::cell::Cell<bool>> = Global(std::cell::Cell::new(false));
    if DONE.0.replace(true) {
        return;
    }
    let dir = game_dir();
    let mut candidates = Vec::new();
    for found in plugin_lib::scan(&dir.join("cl_dlls"), plugin_lib::Target::HOST, "client") {
        match found.outcome {
            plugin_lib::Outcome::Skip(why) => say(&why),
            plugin_lib::Outcome::Load(name) => candidates.push(Candidate { name, file: found.file, library: found.path }),
        }
    }
    candidates.extend(with_gate(|g| g.packaged(&dir)));
    for c in candidates {
        load_one(&c, open);
    }
    let wait = with_gate(|g| g.waits_for_server());
    with(|s| {
        s.open = Some(open);
        s.server_wait = wait;
    });
}

/// What [`load_all`] opened libraries with, for a plugin approved later (a server's offer,
/// client-plugins.md §9); `None` before `Initialize`'s `load_all`.
pub fn opener() -> Option<Open> {
    with(|s| s.open)
}

/// Asks the gate again whether a plugin waits for some server (a server's offer approved one
/// "on this server only" after start).
pub fn refresh_server_wait() {
    let wait = with_gate(|g| g.waits_for_server());
    with(|s| s.server_wait = wait);
}

/// Asks the gate about one candidate and loads it if approved: also the entry point for a plugin
/// approved after start (issue #297, client-plugins.md §9), which then gets `init` at once and
/// `vid_init` if a map is loaded. False if it didn't load.
pub fn load_one(c: &Candidate, open: Open) -> bool {
    if with(|s| s.exts.iter().any(|e| e.name == c.name)) {
        say(&format!("{}: an extension named \"{}\" is already loaded, skipped", c.file, c.name));
        return false;
    }
    let approved = match with_gate(|g| g.check_library(c)) {
        Ok(a) => a,
        Err(why) => {
            say(&format!("{}: {why}, skipped", c.file));
            return false;
        }
    };
    let id = with(|s| {
        let id = s.next_id;
        s.next_id += 1;
        let entry = Entry {
            id,
            name: c.name.clone(),
            library: c.library.clone(),
            approved: approved.clone(),
            hooks: None,
            suspended: false,
            reason: String::new(),
            hud_hidden: 0,
        };
        s.exts.push(entry);
        id
    });
    match open(c, id) {
        Ok((hooks, desc)) => {
            with(|s| s.exts.iter_mut().find(|e| e.id == id).map(|e| e.hooks = Some(hooks.clone())));
            push_hidden();
            say(&format!("loaded {} ({desc}; {}, approved by {})", c.file, approved.id, approved.source));
            let (hud, vid) = with(|s| (s.hud_init, s.vid_init));
            if hud {
                hooks.init();
                if vid {
                    hooks.vid_init();
                }
            }
            true
        }
        Err(why) => {
            with(|s| s.exts.retain(|e| e.id != id));
            say(&format!("{}: {why}, skipped", c.file));
            false
        }
    }
}

/// The checks after an extension's `TSRS_ClientExtLoad` returned `ret` and filled `desc`: it must
/// have returned `OK`, kept the descriptor's size and been built for this major version.
pub fn check_loaded(ret: std::ffi::c_int, desc: &ts_client_ext_api::ClientExt) -> Result<(), String> {
    use ts_client_ext_api::{ClientExt, VERSION};
    if ret != OK {
        return Err(format!("its TSRS_ClientExtLoad refused to load ({ret})"));
    }
    if desc.size as usize != size_of::<ClientExt>() {
        return Err(format!("its extension descriptor is {} bytes, this client's is {}", desc.size, size_of::<ClientExt>()));
    }
    if desc.api_version != VERSION {
        return Err(format!("built for client extension API version {}, this client has version {VERSION}", desc.api_version));
    }
    Ok(())
}

/// The descriptor the client hands an extension to fill: its size set, nothing else (an
/// extension that doesn't say which API version it was built for isn't loaded).
pub fn blank_descriptor() -> ts_client_ext_api::ClientExt {
    ts_client_ext_api::ClientExt { api_version: 0, ..ts_client_ext_api::ClientExt::EMPTY }
}

/// The loaded extensions' names, in load order (`ts_dev_extlist`, tests).
pub fn loaded() -> Vec<String> {
    with(|s| s.exts.iter().filter(|e| e.hooks.is_some()).map(|e| e.name.clone()).collect())
}

fn name_of(id: ExtId) -> Option<String> {
    with(|s| s.exts.iter().find(|e| e.id == id).map(|e| e.name.clone()))
}

fn is_suspended(id: ExtId) -> bool {
    with(|s| s.exts.iter().any(|e| e.id == id && e.suspended))
}

// ---- the client's exports -----------------------------------------------------------------------

/// The end of `HUD_Init`.
pub fn init() {
    engine().add_command(c"ts_dev_extsuspend", cmd_dev_suspend);
    engine().add_command(c"ts_dev_extlist", cmd_dev_list);
    engine().add_command(c"ts_dev_key", cmd_dev_key);
    DEV_EXT_LOG.0.set(Cvar::register(c"ts_dev_extlog", c"0", 0));
    goldsrc::client::set_command_filter(Some(command_filter));
    goldsrc::ext::hud::set_server_cmd_filter(Some(server_cmd_filter));
    with(|s| s.hud_init = true);
    for h in active() {
        h.init();
    }
}

/// The end of `HUD_VidInit`.
pub fn vid_init() {
    with(|s| s.vid_init = true);
    for h in active() {
        h.vid_init();
    }
}

/// The end of `HUD_Frame`: the server check, then `frame`, then the cursor question.
pub fn frame(time: f64) {
    with(|s| s.frame += 1);
    if with(|s| s.exts.is_empty() && !s.server_wait) {
        return;
    }
    let server = engine().server_address();
    if with(|s| s.server != server) {
        server_changed(server);
    }
    for h in active() {
        h.frame(time);
    }
    with_holds(|h, ask, warn| h.tick(now_ms(), ask, warn));
    menus_frame();
    let cursor = active().iter().any(|h| h.wants_cursor());
    with(|s| s.cursor = cursor);
}

/// A new server (or none): suspended extensions resume first (a server's suspension ends with
/// its connection, client-plugins.md §13.3), then every extension hears of it.
fn server_changed(server: Option<String>) {
    let address = CString::new(server.clone().unwrap_or_default()).unwrap_or_default();
    // What the last connection's extensions held isn't displayed on the next.
    let closed = with(|s| {
        s.server = server.clone();
        s.holds.clear();
        s.deaths.clear();
        s.takeovers.reset()
    });
    tell_closed(closed);
    // Plugins approved for this server only load now, and stay loaded (§9); they hear of the
    // server below like the others.
    if let Some(open) = with(|s| s.open) {
        for c in with_gate(|g| g.server_changed(server.as_deref())) {
            load_one(&c, open);
        }
        let wait = with_gate(|g| g.waits_for_server());
        with(|s| s.server_wait = wait);
    }
    resume_all();
    for h in active() {
        h.server_changed(&address);
    }
}

/// `HUD_Redraw`, after the port's HUD and menus.
pub fn redraw(time: f32, intermission: bool) {
    let list = active();
    if list.is_empty() {
        return;
    }
    goldsrc::ext::gl::with_saved_state(|| {
        for h in &list {
            h.redraw(time, intermission);
        }
    });
}

/// The start of `HUD_Key_Event`: true if an extension swallowed the key.
pub fn key_event(down: bool, key: i32, binding: &CStr) -> bool {
    active().iter().any(|h| h.key_event(down, key, binding))
}

/// `IN_ActivateMouse` / `IN_DeactivateMouse`.
pub fn mouse_active(active_now: bool) {
    for h in active() {
        h.mouse_active(active_now);
    }
}

/// An extension asked for the cursor at the last frame (`ClientExt::wants_cursor`).
pub fn cursor_wanted() -> bool {
    with(|s| s.cursor && s.exts.iter().any(|e| !e.suspended))
}

/// The end of `CL_CreateMove`.
pub fn usercmd(cmd: &goldsrc::sys::usercmd_t, active_cmd: bool) {
    let buttons = cmd.buttons as u32;
    let prev = with(|s| std::mem::replace(&mut s.prev_buttons, buttons));
    let list = active();
    if list.is_empty() {
        return;
    }
    let (pressed, released) = events::edges(prev, buttons);
    let c = UserCmd {
        buttons,
        pressed,
        released,
        viewangles: cmd.viewangles,
        forwardmove: cmd.forwardmove,
        sidemove: cmd.sidemove,
        upmove: cmd.upmove,
        impulse: cmd.impulse as i32,
        weaponselect: cmd.weaponselect as i32,
        active: active_cmd as i32,
    };
    for h in list {
        h.usercmd(&c);
    }
}

/// After the HUD handled message `m`: the message's own event, then what it changed.
pub fn on_hud_message(m: shared::protocol::Msg, buf: &[u8]) {
    if with(|s| s.exts.is_empty()) {
        return;
    }
    note_deaths(m, buf);
    if m == Msg::ResetHUD {
        let closed = with(|s| s.takeovers.reset());
        tell_closed(closed);
    }
    let local = engine().local_player().map_or(0, |p| p.index);
    let map =
        if m == shared::protocol::Msg::InitHUD { events::map_name(&goldsrc::ext::render::level_name()).to_owned() } else { String::new() };
    let mut list: Vec<GameEvent> = events::from_message(m, buf, local, &map).into_iter().collect();
    let now = crate::hud::with(|h| events::Snapshot { health: h.last_hp, armor: h.armor as i32, weapon: h.weapon, team: 0 });
    let now = events::Snapshot { team: crate::state::with(|s| s.team_number), ..now };
    let old = with(|s| std::mem::replace(&mut s.snapshot, now));
    list.extend(events::changes(old, now, view::weapon_name));
    let hooks = active();
    for e in &list {
        for h in &hooks {
            h.game_event(e);
        }
    }
}

/// What a message says about other players' deaths ([`alive`]).
fn note_deaths(m: Msg, buf: &[u8]) {
    use shared::protocol::hud as p;
    match m {
        Msg::DeathMsg => {
            let Some(d) = p::DeathMsg::decode(buf) else { return };
            let packet = engine().local_player().map_or(0, |me| me.curstate.messagenum);
            with(|s| s.deaths.died(d.victim as i32, packet));
        }
        Msg::Spectator => {
            let Some(sp) = p::Spectator::decode(buf) else { return };
            with(|s| s.deaths.spectator(sp.client as i32));
        }
        Msg::InitHUD => with(|s| s.deaths.clear()),
        _ => {}
    }
}

/// Whether player `slot` is dead ([`alive::Deaths::is_dead`]): its entity last came in packet
/// `messagenum` with `movetype`.
pub fn player_dead(slot: i32, messagenum: i32, movetype: i32) -> bool {
    with(|s| s.deaths.is_dead(slot, messagenum, movetype))
}

/// `HUD_Shutdown`: every extension, suspended or not.
pub fn shutdown() {
    for h in all() {
        h.shutdown();
    }
}

// ---- suspension ---------------------------------------------------------------------------------

/// Suspends extension `name` (a server's `block` with `suspend`, client-plugins.md §13.3): it
/// gets `suspend(reason)` and nothing else until [`resume`]. False if there's no such loaded,
/// active extension.
pub fn suspend(name: &str, reason: &str) -> bool {
    let hooks = with(|s| {
        let e = s.exts.iter_mut().find(|e| e.name == name && !e.suspended && e.hooks.is_some())?;
        e.suspended = true;
        e.reason = reason.to_owned();
        Some((e.id, e.hooks.clone()?))
    });
    let Some((id, h)) = hooks else { return false };
    // Its holds pass, and it isn't asked again; the menus it took over and the HUD elements it hid
    // come back (fail open, issue #314).
    with_holds(|holds, ask, warn| holds.drop_ext(id, now_ms(), ask, warn));
    give_back(|t| t.ext == id);
    push_hidden();
    h.suspend(&CString::new(reason).unwrap_or_default());
    true
}

/// Resumes a suspended extension. False if it wasn't suspended.
pub fn resume(name: &str) -> bool {
    let hooks = with(|s| {
        let e = s.exts.iter_mut().find(|e| e.name == name && e.suspended)?;
        e.suspended = false;
        e.reason.clear();
        e.hooks.clone()
    });
    let Some(h) = hooks else { return false };
    push_hidden();
    h.resume();
    true
}

/// The suspended extensions and why, by name.
pub fn suspended() -> Vec<(String, String)> {
    with(|s| s.exts.iter().filter(|e| e.suspended).map(|e| (e.name.clone(), e.reason.clone())).collect())
}

fn resume_all() {
    let names: Vec<String> = with(|s| s.exts.iter().filter(|e| e.suspended).map(|e| e.name.clone()).collect());
    for n in names {
        if resume(&n) {
            say(&format!("{n} resumed (a suspension lasts while on the server that asked for it)"));
        }
    }
}

// ---- the table's host functions -----------------------------------------------------------------

/// `ClientExtApi::print`: `[tsmod_<name>] msg`, a newline added if missing.
pub fn host_print(id: ExtId, msg: &[u8]) {
    let name = name_of(id).unwrap_or_else(|| "?".into());
    let mut line = format!("[tsmod_{name}] {}", String::from_utf8_lossy(msg));
    if !line.ends_with('\n') {
        line.push('\n');
    }
    engine().print(&line);
}

/// A console command or cvar name an extension may register: printable, no spaces, quotes or
/// `;`, 1..63 bytes.
pub fn valid_command(name: &str) -> bool {
    (1..64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b';')
}

/// A `'static` copy of a string the engine keeps (command and cvar names, cvar defaults): the
/// engines store the pointer, and extensions' strings aren't promised to live.
fn leak(s: &str) -> &'static CStr {
    Box::leak(CString::new(s).unwrap_or_default().into_boxed_c_str())
}

/// `ClientExtApi::add_command`.
pub fn host_add_command(id: ExtId, name: &str, f: ts_client_ext_api::CommandFn) -> i32 {
    if name_of(id).is_none() || !valid_command(name) {
        return INVALID;
    }
    let lower = name.to_ascii_lowercase();
    if with(|s| s.commands.iter().any(|c| c.name == lower)) || !Cvar::lookup(&CString::new(name).unwrap_or_default()).is_null() {
        return EXISTS;
    }
    with(|s| s.commands.push(Command { name: lower, ext: id, f }));
    // The engine refuses (with its own warning) a name the port or the engine already has.
    engine().add_command(leak(name), command_trampoline);
    OK
}

/// Every extension command goes through here: the extension is found by the command's name.
extern "C" fn command_trampoline() {
    let name = engine().cmd_argv(0).to_string_lossy().to_ascii_lowercase();
    let Some((id, f)) = with(|s| s.commands.iter().find(|c| c.name == name).map(|c| (c.ext, c.f))) else { return };
    if is_suspended(id) {
        host_print(id, format!("{name}: suspended").as_bytes());
        return;
    }
    f();
}

/// `ClientExtApi::register_cvar`.
pub fn host_register_cvar(id: ExtId, name: &str, value: &str, flags: i32) -> i32 {
    if name_of(id).is_none() || !valid_command(name) {
        return INVALID;
    }
    let lower = name.to_ascii_lowercase();
    if !Cvar::lookup(&CString::new(name).unwrap_or_default()).is_null() || with(|s| s.commands.iter().any(|c| c.name == lower)) {
        return EXISTS;
    }
    Cvar::register(leak(name), leak(value), flags);
    OK
}

/// `ClientExtApi::set_cvar_value`.
pub fn host_set_cvar(name: &CStr, value: f32) -> i32 {
    if Cvar::lookup(name).is_null() {
        return NOT_FOUND;
    }
    engine().cvar_set(name, value);
    OK
}

/// `ClientExtApi::client_cmd` (`server` false) and `server_cmd` (true).
pub fn host_cmd(id: ExtId, cmd: &str, server: bool) -> i32 {
    if name_of(id).is_none() {
        return INVALID;
    }
    if is_suspended(id) {
        return SUSPENDED;
    }
    if server {
        // An extension's own commands (the webview's `wv`) don't pass the other extensions'
        // `server_cmd_pre`: that hook is for the port's commands.
        engine().server_cmd_unfiltered(cmd);
    } else {
        let mut line = cmd.to_owned();
        if !line.ends_with('\n') {
            line.push('\n');
        }
        engine().client_cmd(&line);
    }
    OK
}

/// `ClientExtApi::mouse_position`: the port's cursor while a menu or an extension holds it, else
/// the engine's mouse.
pub fn host_mouse_position() -> (i32, i32) {
    if crate::input::cursor_owned() { crate::input::cursor() } else { engine().mouse_position() }
}

/// `ClientExtApi::server_address`: as the last frame saw it, "" when not connected.
pub fn host_server_address() -> String {
    with(|s| s.server.clone()).unwrap_or_default()
}

/// `<gamedir>/cl_plugins_data/<data_name>`.
pub fn data_dir_of(game_dir: &Path, data_name: &str) -> PathBuf {
    game_dir.join("cl_plugins_data").join(data_name)
}

/// `ClientExtApi::data_dir`: the directory with a trailing separator, created on first use.
pub fn host_data_dir(id: ExtId) -> Result<String, i32> {
    let data = with(|s| s.exts.iter().find(|e| e.id == id).map(|e| e.approved.data_name.clone())).ok_or(INVALID)?;
    let dir = data_dir_of(&game_dir(), &data);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        host_print(id, format!("warning: can't create {}: {e}", dir.display()).as_bytes());
    }
    let mut s = dir.to_string_lossy().into_owned();
    s.push(std::path::MAIN_SEPARATOR);
    Ok(s)
}

/// `ClientExtApi::verified_spawn`: the gate re-checks `path` against the extension's approval,
/// then the process starts in the data directory with `args`. Its process id, or an error code.
pub fn host_verified_spawn(id: ExtId, path: &str, args: &[String]) -> i32 {
    let Some((approved, library)) = with(|s| s.exts.iter().find(|e| e.id == id).map(|e| (e.approved.clone(), e.library.clone()))) else {
        return INVALID;
    };
    if is_suspended(id) {
        return SUSPENDED;
    }
    if path.is_empty() {
        return INVALID;
    }
    let exe = {
        let p = PathBuf::from(path);
        if p.is_absolute() { p } else { game_dir().join(p) }
    };
    if let Err(why) = with_gate(|g| g.check_spawn(&approved, &library, &exe)) {
        host_print(id, format!("verified_spawn refused: {why}").as_bytes());
        return REFUSED;
    }
    let cwd = host_data_dir(id).map(PathBuf::from).unwrap_or_else(|_| game_dir());
    let spawned = std::process::Command::new(&exe)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(mut child) => {
            let pid = child.id() as i32;
            // Reaped on a thread that touches nothing else, so it doesn't stay a zombie.
            std::thread::spawn(move || child.wait());
            host_print(id, format!("started {} (pid {pid})", exe.display()).as_bytes());
            pid.max(1)
        }
        Err(e) => {
            host_print(id, format!("verified_spawn: {} failed: {e}", exe.display()).as_bytes());
            FAILED
        }
    }
}

/// `ClientExtApi::visible_players`, cached for the frame.
pub fn host_visible_players() -> Vec<VisiblePlayer> {
    let frame = with(|s| s.frame);
    if let Some(v) = with(|s| s.visible.as_ref().filter(|(f, _)| *f == frame).map(|(_, v)| v.clone())) {
        return v;
    }
    let v = view::visible_players();
    with(|s| s.visible = Some((frame, v.clone())));
    v
}

// ---- TSExt channels (issue #294) -----------------------------------------------------------------

/// `ClientExtApi::claim_channel`.
pub fn host_claim_channel(id: ExtId, name: &[u8]) -> i32 {
    if name_of(id).is_none() || !shared::protocol::ext::valid_channel(name) {
        return INVALID;
    }
    with(|s| match s.channels.iter().find(|(c, _)| c == name) {
        Some((_, owner)) if *owner == id => OK,
        Some(_) => EXISTS,
        None => {
            s.channels.push((name.to_vec(), id));
            OK
        }
    })
}

/// A `TSExt` message: to the extension that claimed its channel, unless it is suspended. A
/// message on an unclaimed channel, or a malformed one, is dropped without a word (servers may
/// send them to any port client).
pub fn on_tsext(buf: &[u8]) {
    let Some((channel, payload)) = shared::protocol::ext::decode(buf) else { return };
    let Some(owner) = with(|s| s.channels.iter().find(|(c, _)| c == channel).map(|(_, id)| *id)) else { return };
    let Some(h) = active_hooks(owner) else { return };
    h.user_message(&CString::new(channel).unwrap_or_default(), payload);
}

// ---- interception (issue #295) -------------------------------------------------------------------

/// `ts_dev_extlog 1` (port-only development aid): what the interception hooks decided, as
/// `[ts ext]` console lines, for `cargo xtask client-plugins`.
static DEV_EXT_LOG: Global<std::cell::Cell<Cvar>> = Global(std::cell::Cell::new(Cvar::NULL));

/// Prints `[ts ext] <line>` when `ts_dev_extlog` is on.
fn ext_log(line: impl FnOnce() -> String) {
    if DEV_EXT_LOG.0.get().value() != 0.0 {
        engine().print(&format!("[ts ext] {}\n", line()));
    }
}

/// A payload's printable ASCII, for [`ext_log`].
fn printable(b: &[u8]) -> String {
    b.iter().filter(|c| c.is_ascii_graphic() || **c == b' ').take(80).map(|&c| c as char).collect()
}

/// Milliseconds on a monotonic clock: the holds' timeouts.
fn now_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// A console warning about an extension's verdict, once per extension and text.
fn warn_once(id: ExtId, text: String) {
    let first = with(|s| {
        let seen = s.warned.iter().any(|(i, t)| *i == id && *t == text);
        if !seen && s.warned.len() < 256 {
            s.warned.push((id, text.clone()));
        }
        !seen
    });
    if first {
        host_print(id, format!("warning: {text}").as_bytes());
    }
}

/// Asks extension `a.ext` about a message: `None` if it no longer takes part.
fn ask_message(a: &intercept::Ask) -> Option<Verdict> {
    let h = active_hooks(a.ext)?;
    Some(h.message_pre(&MsgCall { name: a.kind.name(), data: a.data, allow: a.allow, token: a.token }))
}

/// Runs `f` on the holds taken out of the state (so the extensions it asks may call the table),
/// then displays what it released.
fn with_holds(f: impl FnOnce(&mut intercept::Holds, &mut intercept::AskFn, &mut intercept::WarnFn) -> Vec<(Msg, Vec<u8>)>) {
    if with(|s| s.holds.is_empty()) {
        return;
    }
    let mut holds = with(|s| std::mem::take(&mut s.holds));
    let out = f(&mut holds, &mut ask_message, &mut warn_once);
    let blocks = holds.take_blocks();
    with(|s| s.holds = holds);
    note_menu_blocks(blocks);
    for (m, p) in out {
        ext_log(|| format!("{} released: {}", m.name().to_string_lossy(), printable(&p)));
        note_menu_shown(m, &p);
        crate::hud::display_released(m, &p);
    }
}

/// A user message from the server, before the HUD: what the extensions make of it
/// ([`intercept::Holds::message`]).
pub fn message_pre(m: Msg, buf: &[u8]) -> Dispatch {
    if intercept::message_allow(m).is_none() {
        return Dispatch::Whole;
    }
    let ids: Vec<ExtId> = active_ids().into_iter().map(|(id, _)| id).collect();
    if ids.is_empty() {
        return Dispatch::Whole;
    }
    let mut holds = with(|s| std::mem::take(&mut s.holds));
    let d = holds.message(m, buf, &ids, now_ms(), &mut ask_message, &mut warn_once);
    let blocks = holds.take_blocks();
    with(|s| s.holds = holds);
    note_menu_blocks(blocks);
    match &d {
        Dispatch::Whole => note_menu_shown(m, buf),
        Dispatch::Split(Some(p)) => note_menu_shown(m, p),
        Dispatch::Split(None) => {}
    }
    if intercept::message_allow(m) != Some(0) {
        ext_log(|| {
            let what = match &d {
                Dispatch::Whole => "passed".to_owned(),
                Dispatch::Split(Some(p)) => format!("rewritten: {}", printable(p)),
                Dispatch::Split(None) => "blocked or held".to_owned(),
            };
            format!("{} {what} (came as: {})", m.name().to_string_lossy(), printable(buf))
        });
    }
    d
}

/// `ClientExtApi::release`: `verdict` a `ts_client_ext_api::verdict` code, `data` the
/// replacement for `REPLACE`.
pub fn host_release(id: ExtId, token: u32, verdict: i32, data: Option<&[u8]>) -> i32 {
    use ts_client_ext_api::verdict as v;
    if name_of(id).is_none() {
        return INVALID;
    }
    let verdict = match (verdict, data) {
        (v::PASS, _) => Verdict::Pass,
        (v::BLOCK, _) => Verdict::Block,
        (v::REPLACE, Some(d)) if d.len() <= ts_client_ext_api::MESSAGE_MAX => Verdict::Replace(d.to_vec()),
        _ => return INVALID,
    };
    let mut found = false;
    with_holds(|h, ask, warn| match h.release(id, token, verdict, now_ms(), ask, warn) {
        Some(out) => {
            found = true;
            out
        }
        None => Vec::new(),
    });
    if found { OK } else { NOT_FOUND }
}

/// The command filter (`goldsrc::client::set_command_filter`): each port client command, before
/// it runs. False swallows it.
fn command_filter(name: &CStr) -> bool {
    let list = active_ids();
    if list.is_empty() {
        return true;
    }
    let lower = name.to_string_lossy().to_ascii_lowercase();
    let own = with(|s| s.commands.iter().any(|c| c.name == lower));
    let allowed = intercept::command_allow(&lower, own);
    let e = engine();
    let argv: Vec<CString> = (0..e.cmd_argc()).map(|i| e.cmd_argv(i).to_owned()).collect();
    let line = CString::new(argv.iter().map(|a| quote(&a.to_string_lossy())).collect::<Vec<_>>().join(" ")).unwrap_or_default();
    for (id, h) in list {
        match h.command_pre(&CmdCall { line: &line, argv: &argv, allow: allowed }) {
            Verdict::Pass => {}
            Verdict::Block if allowed & allow::BLOCK != 0 => {
                ext_log(|| format!("command {lower} blocked"));
                return false;
            }
            other => warn_once(id, format!("command {lower} doesn't take {other:?}: passed")),
        }
    }
    true
}

/// A word as a command line carries it: quoted when it has spaces.
fn quote(w: &str) -> String {
    if w.is_empty() || w.contains(char::is_whitespace) { format!("\"{w}\"") } else { w.to_owned() }
}

/// The server command filter (`goldsrc::ext::hud::set_server_cmd_filter`): each command the
/// port sends to the server. The line to send, or `None`.
fn server_cmd_filter(cmd: &str) -> Option<String> {
    let list = active_ids();
    if list.is_empty() {
        return Some(cmd.to_owned());
    }
    let name = intercept::tokenize(cmd).into_iter().next().unwrap_or_default();
    let allowed = intercept::server_cmd_allow(&name);
    let mut line = cmd.to_owned();
    for (id, h) in list {
        let argv: Vec<CString> = intercept::tokenize(&line).into_iter().map(|w| CString::new(w).unwrap_or_default()).collect();
        let cline = CString::new(line.clone()).unwrap_or_default();
        match h.server_cmd_pre(&CmdCall { line: &cline, argv: &argv, allow: allowed }) {
            Verdict::Pass => {}
            Verdict::Block if allowed & allow::BLOCK != 0 => {
                ext_log(|| format!("server command {cmd:?} blocked"));
                return None;
            }
            Verdict::Replace(new) if allowed & allow::REPLACE != 0 => {
                let new = String::from_utf8_lossy(&new).into_owned();
                match intercept::check_command_line(&name, &new) {
                    Ok(()) => line = new,
                    Err(why) => {
                        warn_once(id, format!("its replacement for server command {name} {why}: blocked"));
                        ext_log(|| format!("server command {cmd:?} blocked (a bad replacement)"));
                        return None;
                    }
                }
            }
            other => warn_once(id, format!("server command {name} doesn't take {other:?}: passed")),
        }
    }
    ext_log(|| format!("server command {cmd:?} sent as {line:?}"));
    Some(line)
}

// ---- HUD control and menu takeover (issue #314) ---------------------------------------------------

/// The HUD elements hidden now: every extension's that isn't suspended (suspension shows them
/// again), for `crate::hud::elements`.
fn push_hidden() {
    let (mask, any) = with(|s| (hidden_mask(s.exts.iter().map(|e| (e.suspended, e.hud_hidden))), !s.exts.is_empty()));
    crate::hud::elements::set_hidden(mask);
    crate::hud::elements::set_tracking(any);
}

/// The union of the masks of the extensions (`suspended`, `mask`) that aren't suspended.
pub fn hidden_mask(exts: impl Iterator<Item = (bool, u64)>) -> u64 {
    exts.filter(|(suspended, _)| !suspended).fold(0, |m, (_, mask)| m | mask)
}

/// `ClientExtApi::hud_set_hidden`.
pub fn host_hud_set_hidden(id: ExtId, mask: u64) -> i32 {
    if name_of(id).is_none() || mask & !ts_client_ext_api::hud::ALL != 0 {
        return INVALID;
    }
    if is_suspended(id) {
        return SUSPENDED;
    }
    with(|s| s.exts.iter_mut().filter(|e| e.id == id).for_each(|e| e.hud_hidden = mask));
    push_hidden();
    ext_log(|| format!("hud hidden by {}: {mask:#x}", name_of(id).unwrap_or_default()));
    OK
}

/// `ClientExtApi::hud_elements`.
pub fn host_hud_elements() -> Vec<ts_client_ext_api::HudElement> {
    crate::hud::elements::list()
}

/// What the takeovers need to know of the game now.
fn menu_ctx() -> takeover::Ctx {
    takeover::Ctx { teamplay: crate::hud::with(|h| h.gamemode) >= 1, now_ms: now_ms() }
}

/// Menus extensions blocked: takeovers.
fn note_menu_blocks(blocks: Vec<(Msg, ExtId, Vec<u8>)>) {
    for (m, ext, p) in blocks {
        let closed = with(|s| s.takeovers.message(m, &p, Some(ext), menu_ctx()));
        ext_log(|| format!("{} taken over by {}", m.name().to_string_lossy(), name_of(ext).unwrap_or_default()));
        tell_closed(closed);
    }
}

/// A menu the port displays (passed, rewritten or released): it replaces a takeover of its kind.
fn note_menu_shown(m: Msg, p: &[u8]) {
    if takeover::kind_of(m).is_none() || with(|s| s.takeovers.is_empty()) {
        return;
    }
    let closed = with(|s| s.takeovers.message(m, p, None, menu_ctx()));
    tell_closed(closed);
}

/// Tells extensions their takeovers ended (`ClientExt::menu_closed`), unless suspended.
fn tell_closed(closed: Vec<takeover::Closed>) {
    for (ext, kind, reason) in closed {
        ext_log(|| format!("menu {kind} of {} closed ({reason})", name_of(ext).unwrap_or_default()));
        if let Some(h) = active_hooks(ext) {
            h.menu_closed(kind, reason);
        }
    }
}

/// The takeovers `pick` chooses fail open: the port displays their menus.
fn give_back(pick: impl FnMut(&takeover::Takeover) -> bool) -> Vec<(ExtId, i32)> {
    let back = with(|s| if s.takeovers.is_empty() { Vec::new() } else { s.takeovers.give_back(pick) });
    let mut who = Vec::new();
    for (ext, kind, chunks) in back {
        ext_log(|| format!("menu {kind} of {} given back to the port", name_of(ext).unwrap_or_default()));
        for (m, p) in chunks {
            crate::hud::display_released(m, &p);
        }
        who.push((ext, kind));
    }
    who
}

/// Once a frame: takeovers whose time ran out end, and each extension that holds one is asked
/// whether it still shows it (`menu_shown`); one that says no gives it back.
fn menus_frame() {
    if with(|s| s.takeovers.is_empty()) {
        return;
    }
    let closed = with(|s| s.takeovers.expire(now_ms()));
    tell_closed(closed);
    let open = with(|s| s.takeovers.open());
    let dropped: Vec<(ExtId, i32)> =
        open.into_iter().filter(|&(ext, kind)| active_hooks(ext).and_then(|h| h.menu_shown(kind)) == Some(false)).collect();
    if dropped.is_empty() {
        return;
    }
    for (ext, kind) in give_back(|t| dropped.contains(&(t.ext, t.kind))) {
        if let Some(h) = active_hooks(ext) {
            h.menu_closed(kind, ts_client_ext_api::menu_closed::GIVEN_BACK);
        }
    }
}

/// `ClientExtApi::menu_reply`: the port's own answer to the menu of `kind` extension `id` took
/// over, if `choice` is one the menu offers.
pub fn host_menu_reply(id: ExtId, kind: i32, choice: &str) -> i32 {
    if name_of(id).is_none() {
        return INVALID;
    }
    if is_suspended(id) {
        return SUSPENDED;
    }
    let teams = crate::hud::with(|h| h.team_names.len());
    let answer = match with(|s| s.takeovers.reply(id, kind, choice, teams)) {
        Ok(a) => a,
        Err(e) => {
            ext_log(|| format!("menu {kind} reply {choice:?} refused ({e})"));
            return e;
        }
    };
    ext_log(|| format!("menu {kind} answered: {answer:?}"));
    match answer {
        takeover::Answer::Server(cmd) => engine().server_cmd(&cmd),
        takeover::Answer::Team(n) => crate::menus::join_team(n),
        takeover::Answer::Close => {}
    }
    OK
}

// ---- dev aids (port additions) ------------------------------------------------------------------

/// `ts_dev_extsuspend <name> <1|0> [reason]` (needs `ts_dev_scriptinput 1` and `sv_cheats 1`, as
/// scripted input does): suspends or resumes an extension, standing in for a server's `block`
/// until that exists (client-plugins.md §13.3). For `cargo xtask client-plugins`.
extern "C" fn cmd_dev_suspend() {
    let e = engine();
    if !crate::input::dev_input_allowed() {
        e.print("ts_dev_extsuspend: needs ts_dev_scriptinput 1 and sv_cheats 1\n");
        return;
    }
    let name = e.cmd_argv(1).to_string_lossy().into_owned();
    let on = e.cmd_argv(2).to_bytes() != b"0";
    let reason = e.cmd_argv(3).to_string_lossy().into_owned();
    let done = if on { suspend(&name, if reason.is_empty() { "ts_dev_extsuspend" } else { &reason }) } else { resume(&name) };
    say(&format!("{name} {}{}", if on { "suspended" } else { "resumed" }, if done { "" } else { ": no change" }));
}

/// `ts_dev_extlist`: the loaded extensions.
extern "C" fn cmd_dev_list() {
    let list = with(|s| {
        s.exts.iter().map(|e| format!("{} ({}{})", e.name, e.approved.id, if e.suspended { ", suspended" } else { "" })).collect::<Vec<_>>()
    });
    say(&format!("{} loaded: {}", list.len(), list.join(", ")));
}

/// `ts_dev_key <key> <1|0>` (needs `ts_dev_scriptinput 1` and `sv_cheats 1`): the engine's
/// `Key_Event`, as a key press or release would, so scripts reach `HUD_Key_Event`.
extern "C" fn cmd_dev_key() {
    let e = engine();
    if !crate::input::dev_input_allowed() {
        e.print("ts_dev_key: needs ts_dev_scriptinput 1 and sv_cheats 1\n");
        return;
    }
    let Ok(key) = e.cmd_argv(1).to_string_lossy().parse::<i32>() else {
        e.print("usage: ts_dev_key <key number> <1|0>\n");
        return;
    };
    e.key_event(key, e.cmd_argv(2).to_bytes() != b"0");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert!(valid_command("webview_toggle"));
        assert!(!valid_command("a b"));
        assert!(!valid_command("a;b"));
        assert!(!valid_command(""));
        assert!(!valid_command(&"x".repeat(64)));
        assert_eq!(data_dir_of(Path::new("/g/ts"), "webview"), Path::new("/g/ts/cl_plugins_data/webview"));
    }

    #[test]
    fn hidden_elements_are_every_active_extensions() {
        assert_eq!(hidden_mask([(false, 0b01), (false, 0b100)].into_iter()), 0b101);
        assert_eq!(hidden_mask([(true, 0b01), (false, 0b100)].into_iter()), 0b100, "a suspended extension's come back");
        assert_eq!(hidden_mask(std::iter::empty()), 0);
    }

    #[test]
    fn a_bad_descriptor_or_refusal_is_rejected() {
        use ts_client_ext_api::ClientExt;
        let good = ClientExt::EMPTY;
        assert_eq!(check_loaded(OK, &good), Ok(()));
        assert!(check_loaded(1, &good).unwrap_err().contains("refused"));
        assert!(check_loaded(OK, &ClientExt { size: 8, ..good }).unwrap_err().contains("descriptor"));
        assert!(check_loaded(OK, &ClientExt { api_version: 2, ..good }).unwrap_err().contains("version 2"));
        assert!(check_loaded(OK, &blank_descriptor()).is_err(), "the extension must say its version");
        assert_eq!(blank_descriptor().size as usize, size_of::<ClientExt>());
    }
}
