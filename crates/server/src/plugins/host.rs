//! What the server does for a plugin (the API's host section): its log lines, its data
//! directory, and its server commands. Commands go through one trampoline ([`command`]): the
//! engine calls a command function without arguments, so the trampoline reads `argv(0)` and finds
//! the plugin's callback by name.

use goldsrc::Global;
use goldsrc::ext::weapons::WeaponsEngine;
use goldsrc::server::engine;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::rc::Rc;
use ts_plugin_api::{EXISTS, INVALID, NO_PLUGIN, OK, PluginId, log};

/// A plugin's server command, called with `argv` (`argv[0]` the name).
pub type Command = Rc<dyn Fn(&[&CStr])>;

struct Registered {
    plugin: PluginId,
    name: String,
    f: Command,
}

static COMMANDS: Global<RefCell<Vec<Registered>>> = Global(RefCell::new(Vec::new()));
/// Names the engine already has from a plugin (a failed load keeps them, doing nothing).
static ENGINE_NAMES: Global<RefCell<Vec<String>>> = Global(RefCell::new(Vec::new()));

/// The log prefix of a plugin: `[tsmod_<name>]` for a loaded one, `[<name>]` for an attached one
/// (version 7), `[plugin]` for `NO_PLUGIN` or an id that isn't a plugin's.
pub fn prefix(plugin: PluginId) -> String {
    match super::name_of(plugin) {
        Some(n) if plugin != NO_PLUGIN => prefix_for(&n, super::is_attached(plugin)),
        _ => "[plugin]".to_owned(),
    }
}

/// [`prefix`] of a plugin named `name`.
pub fn prefix_for(name: &str, attached: bool) -> String {
    if attached { format!("[{name}]") } else { format!("[tsmod_{name}]") }
}

/// The line `PluginApi::log` prints, or `None` when its level hides it (debug without
/// `developer`).
pub fn log_line(prefix: &str, level: i32, msg: &str, developer: bool) -> Option<String> {
    let tag = match level {
        log::WARNING => " warning:",
        log::ERROR => " error:",
        log::DEBUG if !developer => return None,
        _ => "",
    };
    let msg = msg.trim_end_matches('\n');
    Some(format!("{prefix}{tag} {msg}\n"))
}

/// `PluginApi::log`.
pub fn log(plugin: PluginId, level: i32, msg: &str) {
    let dev = engine().cvar_float(c"developer") != 0.0;
    if let Some(line) = log_line(&prefix(plugin), level, msg, dev) {
        engine().print(&line);
    }
}

/// `<gamedir>/tsmod/<name>/`, relative to the engine's working directory as the game directory
/// is (TS reads `bots.txt` the same way).
pub fn data_dir_of(game_dir: &str, name: &str) -> PathBuf {
    PathBuf::from(game_dir).join("tsmod").join(name)
}

/// `PluginApi::data_dir`: the directory with a trailing separator, created on first use, or
/// `INVALID` for `NO_PLUGIN` or an id that isn't a loaded or attached plugin's.
pub fn data_dir(plugin: PluginId) -> Result<String, i32> {
    let name = super::name_of(plugin).filter(|_| plugin != NO_PLUGIN).ok_or(INVALID)?;
    let dir = data_dir_of(&engine().game_dir(), &name);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        engine().print(&format!("{} warning: can't create {}: {e}\n", prefix(plugin), dir.display()));
    }
    let mut s = dir.to_string_lossy().into_owned();
    s.push(std::path::MAIN_SEPARATOR);
    Ok(s)
}

/// A server command name a plugin may register: printable, no spaces or quotes, 1..63 bytes.
pub fn valid_command(name: &str) -> bool {
    (1..64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b';')
}

/// `PluginApi::register_command`: `OK`, `EXISTS` (a plugin has it; the engine's own and the
/// game's names are refused by the engine, which prints a warning) or `INVALID`.
pub fn register_command(plugin: PluginId, name: &str, f: Command) -> i32 {
    if !valid_command(name) {
        return INVALID;
    }
    if COMMANDS.0.borrow().iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
        return EXISTS;
    }
    COMMANDS.0.borrow_mut().push(Registered { plugin, name: name.to_owned(), f });
    let known = ENGINE_NAMES.0.borrow().iter().any(|n| n.eq_ignore_ascii_case(name));
    if !known {
        ENGINE_NAMES.0.borrow_mut().push(name.to_owned());
        // The engine keeps the name's pointer for the process.
        let leaked: &'static CStr = Box::leak(CString::new(name).unwrap_or_default().into_boxed_c_str());
        engine().add_server_command(leaked, command);
    }
    OK
}

/// `PluginApi::register_cvar`: the engine's cvar of that name (an existing one, or a new one,
/// leaked for the process), or a null pointer for a name that isn't a valid command name.
pub fn register_cvar(name: &str, value: &str, flags: i32) -> *mut goldsrc::sys::cvar_t {
    if !valid_command(name) {
        return std::ptr::null_mut();
    }
    let eng = engine();
    let c = CString::new(name).unwrap_or_default();
    let existing = eng.cvar_lookup(&c);
    if !existing.is_null() {
        return existing;
    }
    let c: *const goldsrc::sys::cvar_t = eng.register_cvar(name, value, flags);
    c.cast_mut()
}

/// `register_bot_backend`'s checks: a plugin with an id (loaded, or attached since version 7), and
/// a backend name (as a plugin name) other than `native`. `INVALID` otherwise; whether the name is
/// free is the registry's.
pub fn check_backend(plugin: PluginId, name: &str) -> Result<(), i32> {
    let loaded = plugin != NO_PLUGIN && super::name_of(plugin).is_some();
    let ok = loaded && super::loader::valid_name(name) && !name.eq_ignore_ascii_case(crate::bot_backend::NATIVE);
    ok.then_some(()).ok_or(INVALID)
}

/// Drops the commands of a plugin that failed to load, or detached (version 7): the engine keeps
/// the names, which then do nothing (until a plugin registers them again).
pub fn forget(plugin: PluginId) {
    COMMANDS.0.borrow_mut().retain(|c| c.plugin != plugin);
}

/// The trampoline every plugin command is registered with.
extern "C" fn command() {
    let eng = engine();
    let argv: Vec<&CStr> = (0..eng.cmd_argc()).map(|i| eng.cmd_argv(i)).collect();
    let Some(name) = argv.first().map(|n| n.to_string_lossy().into_owned()) else { return };
    let f = COMMANDS.0.borrow().iter().find(|c| c.name.eq_ignore_ascii_case(&name)).map(|c| c.f.clone());
    if let Some(f) = f {
        f(&argv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_lines() {
        assert_eq!(log_line("[tsmod_x]", log::INFO, "hello\n", false).unwrap(), "[tsmod_x] hello\n");
        assert_eq!(log_line("[tsmod_x]", log::WARNING, "w", false).unwrap(), "[tsmod_x] warning: w\n");
        assert_eq!(log_line("[tsmod_x]", log::ERROR, "e", false).unwrap(), "[tsmod_x] error: e\n");
        assert_eq!(log_line("[tsmod_x]", log::DEBUG, "d", false), None);
        assert_eq!(log_line("[tsmod_x]", log::DEBUG, "d", true).unwrap(), "[tsmod_x] d\n");
    }

    #[test]
    fn prefixes() {
        assert_eq!(prefix_for("rcbot", false), "[tsmod_rcbot]");
        assert_eq!(prefix_for("rcbot_amxx", true), "[rcbot_amxx]");
    }

    #[test]
    fn data_dirs_and_command_names() {
        assert_eq!(data_dir_of("ts", "rcbot"), PathBuf::from("ts").join("tsmod").join("rcbot"));
        assert!(valid_command("rcbot"));
        assert!(valid_command("tsmod_test_status"));
        assert!(!valid_command(""));
        assert!(!valid_command("a b"));
        assert!(!valid_command("a;b"));
        assert!(!valid_command(&"x".repeat(64)));
    }
}
