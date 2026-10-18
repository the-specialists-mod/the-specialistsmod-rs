//! Loading the `tsmod_*` plugins and calling them (port addition, issue #104): the FFI half of
//! `ts_server::plugins`. Which files to load, the checks on the descriptor and when each callback
//! runs are decided there; this module opens the library, calls its `TSRS_PluginLoad`, and turns
//! the C callbacks it gets into `ts_server::plugins::PluginHooks` and
//! `ts_server::bot_backend::BotBackend` objects.

use goldsrc::ext::weapons::WeaponsEngine;
use goldsrc::server::engine;
use goldsrc::sys::edict_t;
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use ts_plugin_api::{BotBackend, EXISTS, LOAD_SYMBOL, LoadFn, LoadInfo, OK, Plugin, PluginId};
use ts_server::bot_backend::{self, AddBot};
use ts_server::plugins::{self as reg, PluginHooks, loader};

/// `GameDLLInit`: loads every plugin in `<gamedir>/dlls`, once per process.
pub fn load_all() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    let eng = engine();
    let dir = PathBuf::from(eng.game_dir()).join("dlls");
    for found in loader::scan(&dir, loader::Target::HOST) {
        match found.outcome {
            loader::Outcome::Skip(why) => eng.print(&format!("[ts] plugins: {why}\n")),
            loader::Outcome::Load(name) => match load(&found.path, &name) {
                Ok(desc) => eng.print(&format!("[ts] plugins: loaded {} ({desc})\n", found.file)),
                Err(why) => eng.print(&format!("[ts] plugins: {}: {why}, skipped\n", found.file)),
            },
        }
    }
}

/// A descriptor's string, for the log.
///
/// # Safety
/// `p` is NULL or a NUL-terminated string (the descriptor's contract: static in the library).
unsafe fn text(p: *const c_char) -> String {
    // SAFETY: the caller's guarantee.
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// Opens one library and runs its load function. The library stays mapped for the process once
/// its load function has run (it may have handed out pointers into itself even when it refused).
fn load(path: &Path, name: &str) -> Result<String, String> {
    // SAFETY: opening runs the library's initialisers; a `tsmod_*` file in the server's `dlls/`
    // is a plugin the server's owner installed, and the loader checked its platform first.
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| format!("can't be loaded ({e})"))?;
    // SAFETY: `TSRS_PluginLoad` has the type `LoadFn` by the plugin contract; the pointer is
    // copied out and only used while the library stays mapped (for the process, below).
    let entry: LoadFn = match unsafe { lib.get::<LoadFn>(LOAD_SYMBOL.to_bytes_with_nul()) } {
        Ok(f) => *f,
        Err(_) => return Err(format!("has no {} export", LOAD_SYMBOL.to_string_lossy())),
    };
    // Version 7: an attached plugin (a metamod plugin, at `Meta_Attach`) may have the name.
    let id = reg::reserve(name).ok_or_else(|| format!("the plugin name \"{name}\" is taken"))?;
    let cname = CString::new(name).unwrap_or_default();
    let cpath = CString::new(path.to_string_lossy().into_owned()).unwrap_or_default();
    let info = LoadInfo { id, name: cname.as_ptr(), path: cpath.as_ptr() };
    let mut desc = loader::blank_descriptor();
    let api = crate::plugin_api::table();
    // SAFETY: `entry` is the plugin's load function; `api` is the static table, `info` and `desc`
    // live for the call, and `desc` is `size_of::<Plugin>()` bytes as passed.
    let rc = unsafe { entry(api, api.size, &info, &mut desc, size_of::<Plugin>() as u32) };
    std::mem::forget(lib);
    if let Err(why) = loader::check_loaded(rc, &desc) {
        reg::forget(id);
        return Err(why);
    }
    // SAFETY: the descriptor's strings are NULL or static NUL-terminated strings (its contract).
    let (title, version) = unsafe { (text(desc.name), text(desc.version)) };
    reg::attach(id, Rc::new(Loaded { desc }));
    let title = if title.is_empty() { name.to_owned() } else { title };
    Ok(if version.is_empty() { title } else { format!("{title} {version}") })
}

/// A loaded plugin's callbacks. The safe ones are called directly; each `unsafe` one gets its
/// arguments as the descriptor's contract says.
struct Loaded {
    desc: Plugin,
}

impl PluginHooks for Loaded {
    fn game_init(&self) {
        if let Some(f) = self.desc.game_init {
            f();
        }
    }
    fn precache(&self) {
        if let Some(f) = self.desc.precache {
            f();
        }
    }
    fn server_activate(&self, map: &CStr) {
        if let Some(f) = self.desc.server_activate {
            // SAFETY: `map` is a NUL-terminated string that lives for the call.
            unsafe { f(map.as_ptr()) };
        }
    }
    fn server_deactivate(&self) {
        if let Some(f) = self.desc.server_deactivate {
            f();
        }
    }
    fn start_frame(&self) {
        if let Some(f) = self.desc.start_frame {
            f();
        }
    }
    fn shutdown(&self) {
        if let Some(f) = self.desc.shutdown {
            f();
        }
    }
    fn client_connect(&self, player: i32, bot: bool) {
        if let Some(f) = self.desc.client_connect {
            f(player, bot as c_int);
        }
    }
    fn client_put_in_server(&self, player: i32, bot: bool) {
        if let Some(f) = self.desc.client_put_in_server {
            f(player, bot as c_int);
        }
    }
    fn client_disconnect(&self, player: i32, bot: bool) {
        if let Some(f) = self.desc.client_disconnect {
            f(player, bot as c_int);
        }
    }
    fn wants_client_commands(&self) -> bool {
        self.desc.client_command.is_some()
    }
    fn client_command(&self, player: i32, argv: &[&CStr]) -> bool {
        let Some(f) = self.desc.client_command else { return false };
        let ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        // SAFETY: `ptrs` holds `argc` NUL-terminated strings that live for the call.
        unsafe { f(player, ptrs.len() as c_int, ptrs.as_ptr()) != 0 }
    }
    fn player_spawned(&self, player: i32) {
        if let Some(f) = self.desc.player_spawned {
            f(player);
        }
    }
    fn damage(&self, event: &ts_plugin_api::DamageEvent) {
        if let Some(f) = self.desc.damage {
            // SAFETY: `event` is a `DamageEvent` that lives for the call.
            unsafe { f(event) };
        }
    }
    fn player_killed(&self, victim: i32, attacker: i32, inflictor: i32) {
        if let Some(f) = self.desc.player_killed {
            f(victim, attacker, inflictor);
        }
    }
    fn on_kill(&self, killer: i32, victim: i32, points: i32) {
        if let Some(f) = self.desc.on_kill {
            f(killer, victim, points);
        }
    }
    fn round_start(&self) {
        if let Some(f) = self.desc.round_start {
            f();
        }
    }
    fn round_end(&self) {
        if let Some(f) = self.desc.round_end {
            f();
        }
    }
    fn wants_sounds(&self) -> bool {
        self.desc.sound.is_some()
    }
    fn sound(&self, entity: i32, origin: [f32; 3], sample: &CStr, volume: f32, attenuation: f32, channel: i32, source: i32) {
        if let Some(f) = self.desc.sound {
            // SAFETY: `origin` is three floats and `sample` a NUL-terminated string, both living
            // for the call.
            unsafe { f(entity, origin.as_ptr(), sample.as_ptr(), volume, attenuation, channel, source) };
        }
    }
    fn weapon_changed(&self, player: i32, weapon: i32) {
        if let Some(f) = self.desc.weapon_changed {
            f(player, weapon);
        }
    }
    fn ammo_changed(&self, player: i32, weapon: i32, clip: i32, ammo: i32) {
        if let Some(f) = self.desc.ammo_changed {
            f(player, weapon, clip, ammo);
        }
    }
    fn player_stunt(&self, player: i32, stunt: i32) {
        if let Some(f) = self.desc.player_stunt {
            f(player, stunt);
        }
    }
    fn player_powerup(&self, player: i32, powerup: i32) {
        if let Some(f) = self.desc.player_powerup {
            f(player, powerup);
        }
    }
    fn kill_scored(&self, kill: &ts_plugin_api::KillInfo) {
        if let Some(f) = self.desc.kill_scored {
            // SAFETY: `kill` is a `KillInfo` that lives for the call.
            unsafe { f(kill) };
        }
    }
    fn wants_entity_touch(&self) -> bool {
        self.desc.entity_touch.is_some()
    }
    fn entity_touch(&self, player: i32, other: i32) {
        if let Some(f) = self.desc.entity_touch {
            f(player, other);
        }
    }
    fn weapon_fired(&self, player: i32, weapon: i32, origin: [f32; 3], dir: [f32; 3], silenced: bool) {
        if let Some(f) = self.desc.weapon_fired {
            // SAFETY: `origin` and `dir` are three floats each that live for the call.
            unsafe { f(player, weapon, origin.as_ptr(), dir.as_ptr(), silenced as c_int) };
        }
    }
    fn item_pickup(&self, player: i32, entity: i32, kind: i32, id: i32) {
        if let Some(f) = self.desc.item_pickup {
            f(player, entity, kind, id);
        }
    }
    fn client_plugin_state(&self, player: i32, id: &CStr, version: &CStr, state: i32, reason: &CStr) {
        if let Some(f) = self.desc.client_plugin_state {
            // SAFETY: the three strings are NUL-terminated and live for the call.
            unsafe { f(player, id.as_ptr(), version.as_ptr(), state, reason.as_ptr()) };
        }
    }
    fn client_plugin_list(&self, player: i32, plugins: &[ts_plugin_api::ClientPlugin]) {
        if let Some(f) = self.desc.client_plugin_list {
            // SAFETY: `plugins` holds `len` entries whose strings live for the call
            // (`ts_server::plugins::client_plugin_list`).
            unsafe { f(player, plugins.as_ptr(), plugins.len() as c_int) };
        }
    }
    fn wants_melee_attack(&self) -> bool {
        self.desc.melee_attack.is_some()
    }
    fn melee_attack(&self, player: i32, time: &mut f32, damage: &mut f32) -> bool {
        let Some(f) = self.desc.melee_attack else { return false };
        // SAFETY: `time` and `damage` are exclusive floats that live for the call.
        unsafe { f(player, time, damage) != 0 }
    }
}

/// `PluginApi::add_listener` after its pointer check: a metamod plugin's descriptor, called as a
/// loaded plugin's is, owned by `plugin` (version 7: `NO_PLUGIN` or an attached plugin). `INVALID`
/// for a wrong size or version, or another owner.
pub fn add_listener(plugin: PluginId, desc: Plugin) -> c_int {
    if desc.size as usize != size_of::<Plugin>() || desc.api_version != ts_plugin_api::VERSION {
        return ts_plugin_api::INVALID;
    }
    reg::add_listener(plugin, Rc::new(Loaded { desc })).unwrap_or(ts_plugin_api::INVALID)
}

/// `PluginApi::register_bot_backend` after its pointer checks.
pub fn register_bot_backend(plugin: PluginId, name: &str, cb: BotBackend) -> c_int {
    if let Err(e) = reg::host::check_backend(plugin, name) {
        return e;
    }
    let b = Rc::new(PluginBackend { name: name.to_owned(), cb });
    if bot_backend::register_plugin(plugin, b) { OK } else { EXISTS }
}

/// A plugin's bot backend: the host asks it for bots; everything else the plugin hears through its
/// own callbacks.
struct PluginBackend {
    name: String,
    cb: BotBackend,
}

impl bot_backend::BotBackend for PluginBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn activate(&self) {
        if let Some(f) = self.cb.activate {
            f();
        }
    }

    fn deactivate(&self) {
        if let Some(f) = self.cb.deactivate {
            f();
        }
    }

    fn add_bot(&self, req: &AddBot) -> Option<(*mut edict_t, Option<f32>)> {
        let f = self.cb.add_bot?;
        let (name, model, skill) = req.plugin_args();
        // SAFETY: `name` and `model` are NUL-terminated strings that live for the call.
        let i = unsafe { f(name.as_ptr(), model.as_ptr(), skill) };
        let eng = engine();
        (i > 0 && i <= eng.max_clients()).then(|| (eng.player(i), None)).filter(|(e, _)| !e.is_null())
    }

    fn adopted(&self, e: *mut edict_t) {
        if let Some(f) = self.cb.adopt_bot {
            f(engine().index_of(e));
        }
    }
}
