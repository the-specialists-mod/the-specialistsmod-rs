//! Client plugins (port addition, issue #297; docs/implementation/client-plugins.md §4, §5, §7.1,
//! §8, §9): verification at every start, the start-up prompt and the approval gate the client
//! extension loader asks ([`StoreGate`], client-ext-api.md §3).
//!
//! At `Initialize`, [`install`] reads `cl_plugins/approved.toml`, hashes every installed package
//! (`cl_plugins/<id>/<version>/`) and hand-installed library (`cl_dlls/tsmod_<name><suffix>`),
//! asks the player about each new or changed one ([`prompt`]), then installs [`StoreGate`] with
//! `ext::set_gate`; `ts-client-dll` runs `ext::load_all` right after. The gate maps only what an
//! approval covers byte for byte, re-hashing anything a prompt stood in front of, and
//! `verified_spawn` re-hashes each helper against the approval ([`plugin_pkg::Entry::check_file`]).
//!
//! A server's offers ([`offers`], issue #299) add plugins while the game runs: a click on an
//! in-game card downloads, verifies and installs one, records its approval in the store and in
//! the gate's state here, and loads it late.
//!
//! The plugins panel ([`menu`], issue #311) lists them, shows every file's hash, and revokes,
//! unblocks and deletes, each on a click.
//!
//! **Nothing here approves from the console** (§7.3, §8): the store is written only from a
//! click in the start-up box, on an offer card or in the plugins panel (and by tsmod-launcher).
//! `cl_plugins_list` and `cl_plugins_verify` only read; `cl_plugins_cancel` only stops downloads;
//! `cl_plugins_menu` only opens the panel.

pub mod click;
pub mod menu;
pub mod offers;
pub mod prompt;
pub mod startup;

use crate::ext::gate::{self, ApprovalGate, Approved, Candidate};
use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
use plugin_pkg::{Entry, Platform, Store, Version};
use startup::{Decision, Dirs, Item, Options};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `[ts] client plugins:` console prefix.
fn say(line: &str) {
    engine().print(&format!("[ts] client plugins: {line}\n"));
}

struct Loaded {
    /// The allowed entry it was mapped under, and the directory its paths are relative to.
    entry: Entry,
    base: PathBuf,
}

struct State {
    dirs: Dirs,
    opts: Options,
    store: Store,
    store_writable: bool,
    items: Vec<Item>,
    /// Library path → index in `items`.
    by_library: BTreeMap<PathBuf, usize>,
    /// Mapped libraries, by library path (`check_spawn`).
    loaded: BTreeMap<PathBuf, Loaded>,
    /// The server the loader last told the gate about.
    server: Option<String>,
}

static STATE: Global<RefCell<Option<State>>> = Global(RefCell::new(None));

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.0.borrow_mut().as_mut().map(f)
}

/// The running port's version (`_tsrs`), for `[requires] port` (§13.5).
pub fn port_version() -> Version {
    shared::protocol::port::VERSION.parse().unwrap_or(Version::new(0, 0, 0))
}

/// Whether `library` is a library this client can map (its header names this platform and
/// architecture, as the loader's own check does).
pub fn header_ok(library: &Path) -> Result<(), String> {
    use std::io::Read;
    let mut buf = Vec::new();
    let read = std::fs::File::open(library).and_then(|f| f.take(shared::plugin_lib::HEADER_BYTES as u64).read_to_end(&mut buf));
    let host = shared::plugin_lib::Target::HOST;
    match read.ok().map(|_| shared::plugin_lib::image_target(&buf)) {
        None => Err("its library can't be read".into()),
        Some(None) => Err(format!("its library isn't one for this client ({})", host.describe())),
        Some(Some(t)) if t != host => Err(format!("its library is built for {} (this client is {})", t.describe(), host.describe())),
        Some(Some(_)) => Ok(()),
    }
}

/// Reads `cl_plugins` and `cl_plugins_verify_log` from `config.cfg`: the engine executes it after
/// `Initialize`, so their cvars don't have the player's values yet (§8).
fn archived_switch(game_dir: &Path, name: &str, default: bool) -> bool {
    std::fs::read_to_string(game_dir.join("config.cfg"))
        .ok()
        .and_then(|cfg| startup::archived_cvar(&cfg, name))
        .map_or(default, |v| startup::switch(&v))
}

/// `Initialize`, before `ext::load_all`: verification, the start-up prompt, and the gate.
pub fn install() {
    let game_dir = crate::ext::game_dir();
    let Some(platform) = Platform::current() else {
        say("no client plugins on this platform");
        return;
    };
    let (dev_names, dev_note) = gate::dev_override_names(std::env::var(gate::DEV_ENV).ok().as_deref(), cfg!(debug_assertions));
    if let Some(n) = &dev_note {
        engine().print(&format!("[ts] client extensions: {n}\n"));
    }
    let opts = Options {
        enabled: archived_switch(&game_dir, "cl_plugins", true),
        verify_log: archived_switch(&game_dir, "cl_plugins_verify_log", false),
        platform,
        port: port_version(),
        api: ts_client_ext_api::VERSION,
        threads: 0,
        dev_names: dev_names.clone(),
    };
    let dirs = Dirs::of(&game_dir);
    let env = |k: &str| std::env::var(k).ok();
    let (hook, hook_note) =
        prompt::hook(env(prompt::HOOK_ENV).as_deref(), env(prompt::HOOK_LOG_ENV).as_deref(), &game_dir, cfg!(debug_assertions));
    if let Some(n) = hook_note {
        say(&n);
    }
    let mut fallback = prompt::Fallback { told: false, lines: Vec::new() };
    let mut sdl = prompt::SdlPrompter;
    let mut hook = hook;
    let prompter: &mut dyn prompt::Prompter = match hook.as_mut() {
        Some(h) => h,
        None if prompt::sdl_available() => &mut sdl,
        None => &mut fallback,
    };
    let loaded = Store::load(&dirs.store());
    // What an offer's install left behind (staging, set-aside directories, superseded versions).
    if let (true, Ok((store, _))) = (opts.enabled, &loaded) {
        offers::clean_at_start(&dirs.cl_plugins, store, platform);
    }
    let out = startup::run(&dirs, &opts, loaded, prompter, &header_ok);
    for l in out.lines.iter().chain(&fallback.lines) {
        say(l);
    }
    // A package with no library to map never reaches the loader: say why here.
    for it in out.items.iter().filter(|it| it.library.is_none()) {
        if let Decision::Skip(why) = &it.decision {
            say(&format!("{}: {why}", it.candidate.id()));
        }
    }
    let by_library = startup::by_library(&out.items);
    *STATE.0.borrow_mut() = Some(State {
        dirs,
        opts,
        store: out.store,
        store_writable: out.store_writable,
        items: out.items,
        by_library,
        loaded: BTreeMap::new(),
        server: None,
    });
    let (g, _) = gate::with_dev_override(Box::new(StoreGate), std::env::var(gate::DEV_ENV).ok().as_deref(), cfg!(debug_assertions));
    crate::ext::set_gate(g);
}

/// The approval gate over the store (client-ext-api.md §3).
pub struct StoreGate;

fn approved_as(item: &Item, e: &Entry) -> Approved {
    Approved { id: e.id.clone(), data_name: item.name.clone(), source: e.source.clone() }
}

impl ApprovalGate for StoreGate {
    fn packaged(&self, _game_dir: &Path) -> Vec<Candidate> {
        with(|s| {
            s.items
                .iter()
                .filter(|it| matches!(it.candidate, plugin_pkg::Candidate::Package { .. }))
                .filter_map(|it| {
                    let library = it.library.clone()?;
                    let file = library.file_name()?.to_string_lossy().into_owned();
                    Some(Candidate { name: it.name.clone(), file, library })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    fn check_library(&self, c: &Candidate) -> Result<Approved, String> {
        let r = with(|s| {
            let Some(&i) = s.by_library.get(&c.library) else {
                return Err(if s.opts.enabled { "not found at start (restart to verify it)".into() } else { "cl_plugins is 0".into() });
            };
            let (v, fresh) = match &s.items[i].decision {
                Decision::Skip(why) => return Err(why.clone()),
                Decision::Allowed { v, fresh } => ((**v).clone(), *fresh),
            };
            let server = s.server.clone();
            // A verification from just now with nothing in between is used as it is, once;
            // anything else (a prompt was answered, a server-scoped plugin loading on connect)
            // is hashed again right before it's mapped.
            let v = if fresh {
                if let Decision::Allowed { fresh, .. } = &mut s.items[i].decision {
                    *fresh = false;
                }
                startup::usable(v, &s.opts, server.as_deref(), &header_ok)?
            } else {
                startup::recheck(&s.items[i].candidate, &s.opts, &s.store, server.as_deref(), &header_ok)?
            };
            let entry = v.entry.clone().ok_or_else(|| "no approval".to_string())?;
            let approved = approved_as(&s.items[i], &entry);
            s.loaded.insert(c.library.clone(), Loaded { entry, base: v.base.clone() });
            Ok(approved)
        });
        r.unwrap_or_else(|| Err("the approval store isn't set up".into()))
    }

    fn check_spawn(&self, _plugin: &Approved, library: &Path, exe: &Path) -> Result<(), String> {
        with(|s| {
            let l = s.loaded.get(library).ok_or_else(|| "its library isn't loaded under an approval".to_string())?;
            let rel = relative(&l.base, exe).ok_or_else(|| format!("{} isn't one of its approved files", exe.display()))?;
            l.entry.check_file(&l.base, &rel)
        })
        .unwrap_or_else(|| Err("the approval store isn't set up".into()))
    }

    fn server_changed(&self, server: Option<&str>) -> Vec<Candidate> {
        with(|s| {
            s.server = server.map(str::to_owned);
            server_candidates(s)
        })
        .unwrap_or_default()
    }

    fn waits_for_server(&self) -> bool {
        with(|s| s.items.iter().any(|it| waiting(s, it).is_some())).unwrap_or(false)
    }
}

/// The entry of a plugin approved for some servers only and not loaded yet.
fn waiting<'a>(s: &State, it: &'a Item) -> Option<&'a Entry> {
    let Decision::Allowed { v, .. } = &it.decision else { return None };
    let e = v.entry.as_ref()?;
    (e.scope == plugin_pkg::Scope::Server && !s.loaded.contains_key(it.library.as_ref()?)).then_some(e)
}

fn server_candidates(s: &State) -> Vec<Candidate> {
    s.items
        .iter()
        .filter(|it| waiting(s, it).is_some_and(|e| e.applies_to(s.server.as_deref())))
        .filter_map(|it| {
            let library = it.library.clone()?;
            let file = library.file_name()?.to_string_lossy().into_owned();
            Some(Candidate { name: it.name.clone(), file, library })
        })
        .collect()
}

/// `exe` relative to `base`, `/`-joined, both resolved first (so `./x` and links in the game
/// directory's path match). `None` if it isn't under `base`.
pub fn relative(base: &Path, exe: &Path) -> Option<String> {
    let (base, exe) = (base.canonicalize().ok()?, exe.canonicalize().ok()?);
    let rel = exe.strip_prefix(&base).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

// ---- cvars and commands (§8; port additions) -----------------------------------------------------

/// `HUD_Init`: the cvars and the read-only commands.
pub fn init() {
    use goldsrc::ext::render::FCVAR_ARCHIVE;
    // cl_plugins and cl_plugins_verify_log take effect at start, from config.cfg (read at
    // Initialize); registered here so the engine keeps them archived. cl_plugins_download and
    // cl_plugins_allow_unsigned are read when a server offers a plugin (`offers`, #299).
    Cvar::register(c"cl_plugins", c"1", FCVAR_ARCHIVE);
    Cvar::register(c"cl_plugins_download", c"1", FCVAR_ARCHIVE);
    Cvar::register(c"cl_plugins_allow_unsigned", c"0", FCVAR_ARCHIVE);
    Cvar::register(c"cl_plugins_verify_log", c"0", FCVAR_ARCHIVE);
    engine().add_command(c"cl_plugins_list", cmd_list);
    engine().add_command(c"cl_plugins_verify", cmd_verify);
    offers::init();
    menu::init();
}

// ---- the in-game cards and the panel: drawing and input (§7.3) ---------------------------------

/// An offer card or the plugins panel is open: it owns the cursor, and extensions get no `redraw`
/// (§7.3 "No cover").
pub fn covers() -> bool {
    offers::covers() || menu::is_open()
}

/// The cursor is the card's or the panel's while one is open.
pub fn wants_cursor() -> bool {
    covers()
}

/// `HUD_Redraw`, last: the panel, then an open offer card over it.
pub fn redraw() {
    menu::redraw(!offers::covers());
    offers::redraw();
}

/// Mouse buttons while [`wants_cursor`] (`IN_MouseEvent`): an offer card takes them first.
pub fn mouse(state: i32, old: i32, x: i32, y: i32) {
    if offers::covers() {
        offers::mouse(state, old, x, y);
    } else {
        menu::mouse(state, old, x, y);
    }
}

/// `HUD_Key_Event`, first: an offer card's keys, then the panel's. True swallows the key.
pub fn key_event(down: bool, key: i32) -> bool {
    offers::key_event(down, key) || menu::key_event(down, key)
}

/// Every loaded plugin with the version its approval was verified at (`0.0.0` for a
/// hand-installed library), as `tspkg list` reports them (§13.2), and its extension's name.
pub fn loaded_plugins() -> Vec<offers::enforce::LoadedPlugin> {
    let loaded = crate::ext::loaded();
    with(|s| {
        s.items
            .iter()
            .filter(|it| loaded.contains(&it.name))
            .map(|it| {
                let version = match &it.decision {
                    Decision::Allowed { v, .. } => v.version.as_ref().map_or_else(|| "0.0.0".into(), ToString::to_string),
                    Decision::Skip(_) => "0.0.0".into(),
                };
                offers::enforce::LoadedPlugin { id: it.candidate.id(), version, ext: it.name.clone() }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `cl_plugins_list`: every plugin found at start, its approval and whether it loaded (and, on
/// this server, whether it is suspended and why, §13.4).
pub fn list_lines() -> Vec<String> {
    let loaded = crate::ext::loaded();
    let suspended = crate::ext::suspended();
    with(|s| {
        let mut out = vec![format!("{} plugin(s) found at start{}", s.items.len(), if s.opts.enabled { "" } else { " (cl_plugins 0)" })];
        for it in &s.items {
            let id = it.candidate.id();
            let entry = s.store.entries_for(&id).next();
            let version = match &it.decision {
                Decision::Allowed { v, .. } => v.version.as_ref().map(ToString::to_string),
                Decision::Skip(_) => entry.map(|e| e.version.to_string()),
            }
            .unwrap_or_else(|| "?".into());
            let state = entry.map_or_else(|| "not approved".to_string(), entry_text);
            let status = status_text(it, &loaded, &suspended);
            out.push(format!("  {id} {version}: {state}; {status}"));
        }
        if !s.store_writable {
            out.push("  (the approval store couldn't be read)".into());
        }
        out
    })
    .unwrap_or_else(|| vec!["client plugins aren't set up in this client".into()])
}

/// Whether a plugin found at start is loaded (and suspended, and why), or why not; for
/// `cl_plugins_list` and the panel. `loaded` and `suspended` are the extension loader's.
pub fn status_text(it: &Item, loaded: &[String], suspended: &[(String, String)]) -> String {
    if let Some((_, why)) = suspended.iter().find(|(n, _)| *n == it.name) {
        format!("loaded, suspended by this server ({why})")
    } else if loaded.contains(&it.name) {
        "loaded".to_string()
    } else {
        match &it.decision {
            Decision::Skip(why) => format!("not loaded: {why}"),
            Decision::Allowed { v, .. } => match v.entry.as_ref().filter(|e| e.scope == plugin_pkg::Scope::Server) {
                Some(e) => format!("not loaded: {}", startup::server_only(e)),
                None => "not loaded".into(),
            },
        }
    }
}

/// Saves the store after a click changed it; the error is a console line. A store that couldn't
/// be read at start isn't written over (the change applies to this game only).
fn save_store(s: &State) -> Result<(), String> {
    if !s.store_writable {
        return Err(format!("{} couldn't be read at start, so changes aren't saved", s.dirs.store().display()));
    }
    s.store.save(&s.dirs.store()).map_err(|e| format!("can't save {} ({e})", s.dirs.store().display()))
}

/// An approval's state, scope, servers and trust, for `cl_plugins_list`.
pub fn entry_text(e: &Entry) -> String {
    use plugin_pkg::{Scope, State, Trust};
    let state = match e.state {
        State::Allowed => "allowed",
        State::Blocked => "blocked",
    };
    let scope = match e.scope {
        Scope::Always => String::new(),
        Scope::Server => format!(" on {}", e.servers.join(", ")),
    };
    let trust = match e.trust {
        Trust::Verified => "verified",
        Trust::Known => "known publisher",
        Trust::New => "new publisher",
        Trust::Unsigned => "unsigned",
    };
    format!("{state}{scope} ({trust}, from {})", e.source)
}

fn verdict_text(v: plugin_pkg::Verdict) -> &'static str {
    match v {
        plugin_pkg::Verdict::Load => "matches its approval",
        plugin_pkg::Verdict::Blocked => "blocked",
        plugin_pkg::Verdict::Prompt => "needs approval (asked at the next start)",
        plugin_pkg::Verdict::Refuse => "can't be approved as installed",
    }
}

extern "C" fn cmd_list() {
    for l in list_lines() {
        say(&l);
    }
}

/// `cl_plugins_verify`: hashes every plugin again now and prints what it finds. Never approves.
pub fn verify_lines() -> Vec<String> {
    with(|s| {
        let found = plugin_pkg::verify_all(
            &s.dirs.cl_plugins,
            &s.dirs.cl_dlls,
            s.opts.platform.library_suffix(),
            s.opts.platform,
            &s.store,
            &plugin_pkg::builtin_roots(),
            s.opts.threads,
        );
        match found {
            Err(e) => vec![format!("can't list the plugins: {e}")],
            Ok(list) => {
                let mut out = vec![format!("verified {} plugin(s) now (nothing is approved from here)", list.len())];
                for v in list {
                    let ver = v.version.as_ref().map_or_else(|| "?".into(), ToString::to_string);
                    let what = if v.changes.is_empty() { String::new() } else { format!(": {}", startup::change_summary(&v.changes)) };
                    out.push(format!("  {} {ver}: {}, {} file(s){what}", v.id, verdict_text(v.verdict), v.files.len()));
                }
                out
            }
        }
    })
    .unwrap_or_else(|| vec!["client plugins aren't set up in this client".into()])
}

extern "C" fn cmd_verify() {
    for l in verify_lines() {
        say(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_stay_inside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files/bin")).unwrap();
        std::fs::write(dir.path().join("files/bin/host"), b"x").unwrap();
        std::fs::write(dir.path().join("outside"), b"x").unwrap();
        let base = dir.path().join("files");
        assert_eq!(relative(dir.path(), &base.join("bin/./host")).as_deref(), Some("files/bin/host"));
        assert_eq!(relative(&base, &dir.path().join("outside")), None);
        assert_eq!(relative(&base, &base.join("missing")), None);
    }

    #[test]
    fn the_port_version_parses() {
        assert_eq!(port_version().to_string(), shared::protocol::port::VERSION);
    }
}
