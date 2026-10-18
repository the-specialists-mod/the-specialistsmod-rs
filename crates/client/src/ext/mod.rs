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
//!
//! Every dispatch copies the list of extensions out first, so an extension may call the table
//! (which reads this module's state) from any callback.

pub mod events;
pub mod gate;
pub mod view;

use gate::{ApprovalGate, Approved, Candidate};
use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
use shared::plugin_lib;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use ts_client_ext_api::{EXISTS, ExtId, FAILED, GameEvent, INVALID, NOT_FOUND, OK, REFUSED, SUSPENDED, UserCmd, VisiblePlayer};

/// A loaded extension's callbacks. `ts-client-dll` implements it over the extension's C
/// descriptor; every method does nothing unless the extension gave that callback.
pub trait ExtHooks {
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
        let entry =
            Entry { id, name: c.name.clone(), library: c.library.clone(), approved: approved.clone(), hooks: None, suspended: false };
        s.exts.push(entry);
        id
    });
    match open(c, id) {
        Ok((hooks, desc)) => {
            with(|s| s.exts.iter_mut().find(|e| e.id == id).map(|e| e.hooks = Some(hooks.clone())));
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
    let cursor = active().iter().any(|h| h.wants_cursor());
    with(|s| s.cursor = cursor);
}

/// A new server (or none): suspended extensions resume first (a server's suspension ends with
/// its connection, client-plugins.md §13.3), then every extension hears of it.
fn server_changed(server: Option<String>) {
    let address = CString::new(server.clone().unwrap_or_default()).unwrap_or_default();
    with(|s| s.server = server.clone());
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
        e.hooks.clone()
    });
    let Some(h) = hooks else { return false };
    h.suspend(&CString::new(reason).unwrap_or_default());
    true
}

/// Resumes a suspended extension. False if it wasn't suspended.
pub fn resume(name: &str) -> bool {
    let hooks = with(|s| {
        let e = s.exts.iter_mut().find(|e| e.name == name && e.suspended)?;
        e.suspended = false;
        e.hooks.clone()
    });
    let Some(h) = hooks else { return false };
    h.resume();
    true
}

fn resume_all() {
    let names: Vec<String> = with(|s| s.exts.iter().filter(|e| e.suspended).map(|e| e.name.clone()).collect());
    for n in names {
        resume(&n);
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
    if with(|s| s.commands.iter().any(|c| c.name == lower)) || !Cvar::find(&CString::new(name).unwrap_or_default()).is_null() {
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
    if !Cvar::find(&CString::new(name).unwrap_or_default()).is_null() || with(|s| s.commands.iter().any(|c| c.name == lower)) {
        return EXISTS;
    }
    Cvar::register(leak(name), leak(value), flags);
    OK
}

/// `ClientExtApi::set_cvar_value`.
pub fn host_set_cvar(name: &CStr, value: f32) -> i32 {
    if Cvar::find(name).is_null() {
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
        engine().server_cmd(cmd);
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
