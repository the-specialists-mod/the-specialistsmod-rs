//! The approval gate: whether a client extension may be mapped at all, and whether one of its
//! helper executables may start (docs/implementation/client-ext-api.md §3, client-plugins.md §5,
//! §9). Port addition, issue #293.
//!
//! The loader asks the gate about every candidate before it maps anything, and
//! `verified_spawn` asks it before starting a process. The real gate is the approval store
//! (`plugin-pkg`) with the start-up prompt and the package candidates: `crate::plugins`'
//! `StoreGate` (issue #297), which `ts-client-dll` installs with [`super::set_gate`] at
//! `Initialize`, before the loader runs. Without it the gate is [`DenyUnapproved`]: nothing is
//! approved, so nothing loads.
//!
//! **Development override** ([`DevOverride`]): in a debug build only (`debug_assertions`, the
//! profile `cargo xtask install` uses by default), the environment variable
//! [`DEV_ENV`]`=name,name,...` approves the hand-installed `cl_dlls/tsmod_<name>` libraries it
//! names, unhashed, and their helpers listed in `cl_dlls/tsmod_<name>.files`. A release build
//! ignores the variable and says so. It wraps the store's gate ([`with_dev_override`]); `cargo
//! xtask client-plugins` approves its fixtures with `xtask plugin-approve` and keeps one on the
//! override, so both stay tested.

use std::path::{Path, PathBuf};

/// The development override's environment variable (debug builds only; see the module docs).
pub const DEV_ENV: &str = "TSRS_DEV_CLIENT_EXT";

/// A library the loader found and asks the gate about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `<name>` of `tsmod_<name>`.
    pub name: String,
    /// The file name, for messages.
    pub file: String,
    /// The library's path.
    pub library: PathBuf,
}

/// What the gate approved a library as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approved {
    /// The plugin id: a package's `id`, or `manual:<name>` for a hand-installed library
    /// (client-plugins.md §5.3).
    pub id: String,
    /// The directory name under `cl_plugins_data/` ([`crate::ext`]'s `data_dir`).
    pub data_name: String,
    /// For the console: "manual", "server 1.2.3.4:27015", "dev override", ...
    pub source: String,
}

/// The seam the approval store plugs into (issue #297). Every method runs on the engine's
/// thread, at `Initialize` (the library checks) or from an extension's call (`check_spawn`).
pub trait ApprovalGate {
    /// Candidates besides the hand-installed `cl_dlls/tsmod_*` libraries the loader scans
    /// itself: installed packages' libraries (`cl_plugins/<id>/<version>/`, already checked for
    /// the platform). The loader checks each one with [`Self::check_library`] like the others.
    fn packaged(&self, _game_dir: &Path) -> Vec<Candidate> {
        Vec::new()
    }
    /// Whether `c` may be mapped now: every file hashed and matched against the store
    /// (client-plugins.md §5.2). `Err` is the reason, for the console.
    fn check_library(&self, c: &Candidate) -> Result<Approved, String>;
    /// Whether the extension approved as `plugin` (library `library`) may start `exe` now: the
    /// file re-hashed against the approval just before it starts (client-plugins.md §5.2).
    fn check_spawn(&self, plugin: &Approved, library: &Path, exe: &Path) -> Result<(), String>;
    /// The server changed (`None`: disconnected): candidates that may load now, approved for
    /// this server only (`scope = "server"`, client-plugins.md §9). The loader loads each with
    /// `load_one`, which asks [`Self::check_library`] again.
    fn server_changed(&self, _server: Option<&str>) -> Vec<Candidate> {
        Vec::new()
    }
    /// Whether [`Self::server_changed`] can return anything (the loader watches the server only
    /// then, or while extensions are loaded).
    fn waits_for_server(&self) -> bool {
        false
    }
}

/// The default gate: nothing is approved. The approval store (#296, #297) replaces it.
pub struct DenyUnapproved;

impl ApprovalGate for DenyUnapproved {
    fn check_library(&self, _c: &Candidate) -> Result<Approved, String> {
        Err("not approved (the approval store isn't installed in this build)".into())
    }
    fn check_spawn(&self, _plugin: &Approved, _library: &Path, exe: &Path) -> Result<(), String> {
        Err(format!("{} isn't covered by an approval", exe.display()))
    }
}

/// The development override (module docs): approves the hand-installed libraries it names and
/// asks `inner` about the rest.
pub struct DevOverride {
    pub names: Vec<String>,
    pub inner: Box<dyn ApprovalGate>,
}

/// The names in a [`DEV_ENV`] value: comma- or space-separated, empty entries dropped.
pub fn dev_names(value: &str) -> Vec<String> {
    value.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// The paths a `tsmod_<name>.files` list names (client-plugins.md §5.3): one per line, relative
/// to the list's directory; blank lines and `#` comments skipped. Paths that climb out of the
/// directory (`..`) or are absolute are dropped.
pub fn files_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .filter(|p| p.is_relative() && p.components().all(|c| matches!(c, std::path::Component::Normal(_))))
        .collect()
}

/// Whether `exe` is one of the files `list` names relative to `dir` (compared after resolving
/// both, so `./x` and symlinked directories match).
fn listed(dir: &Path, list: &[PathBuf], exe: &Path) -> bool {
    let Ok(exe) = exe.canonicalize() else { return false };
    list.iter().any(|p| dir.join(p).canonicalize().is_ok_and(|q| q == exe))
}

impl ApprovalGate for DevOverride {
    fn packaged(&self, game_dir: &Path) -> Vec<Candidate> {
        self.inner.packaged(game_dir)
    }
    fn server_changed(&self, server: Option<&str>) -> Vec<Candidate> {
        self.inner.server_changed(server)
    }
    fn waits_for_server(&self) -> bool {
        self.inner.waits_for_server()
    }
    fn check_library(&self, c: &Candidate) -> Result<Approved, String> {
        if self.names.contains(&c.name) {
            return Ok(Approved {
                id: format!("manual:{}", c.name),
                data_name: c.name.clone(),
                source: format!("{DEV_ENV} (debug build)"),
            });
        }
        self.inner.check_library(c)
    }
    fn check_spawn(&self, plugin: &Approved, library: &Path, exe: &Path) -> Result<(), String> {
        let name = plugin.id.strip_prefix("manual:").filter(|n| self.names.iter().any(|m| m == n));
        let Some(name) = name else { return self.inner.check_spawn(plugin, library, exe) };
        let dir = library.parent().unwrap_or(Path::new("."));
        let list = std::fs::read_to_string(dir.join(format!("tsmod_{name}.files"))).map(|t| files_list(&t)).unwrap_or_default();
        if listed(dir, &list, exe) { Ok(()) } else { Err(format!("{} isn't listed in tsmod_{name}.files", exe.display())) }
    }
}

/// The gate a build starts with when nothing installed one: [`DenyUnapproved`], under the
/// [`DevOverride`] when this is a debug build and [`DEV_ENV`] is set. The note is for the console.
pub fn default_gate(env: Option<&str>) -> (Box<dyn ApprovalGate>, Option<String>) {
    with_dev_override(Box::new(DenyUnapproved), env, cfg!(debug_assertions))
}

/// The names [`with_dev_override`] approves for `env` in a build with `debug` assertions:
/// none in a release build, and the note for the console either way.
pub fn dev_override_names(env: Option<&str>, debug: bool) -> (Vec<String>, Option<String>) {
    let names = env.map(dev_names).unwrap_or_default();
    if names.is_empty() {
        return (names, None);
    }
    if !debug {
        return (Vec::new(), Some(format!("{DEV_ENV} is ignored in release builds")));
    }
    let note = format!("{DEV_ENV}: approving {} without hashing (debug build only)", names.join(", "));
    (names, Some(note))
}

/// `inner` under the [`DevOverride`] when `debug` (pass `cfg!(debug_assertions)`) and [`DEV_ENV`]
/// names libraries; `inner` alone otherwise. The note is for the console.
pub fn with_dev_override(inner: Box<dyn ApprovalGate>, env: Option<&str>, debug: bool) -> (Box<dyn ApprovalGate>, Option<String>) {
    let (names, note) = dev_override_names(env, debug);
    if names.is_empty() { (inner, note) } else { (Box::new(DevOverride { names, inner }), note) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str) -> Candidate {
        Candidate {
            name: name.into(),
            file: format!("tsmod_{name}_amd64.so"),
            library: PathBuf::from(format!("cl_dlls/tsmod_{name}_amd64.so")),
        }
    }

    #[test]
    fn nothing_loads_by_default() {
        let (g, note) = default_gate(None);
        assert!(note.is_none());
        assert!(g.check_library(&cand("webview")).is_err());
        let (g, _) = default_gate(Some(" , "));
        assert!(g.check_library(&cand("webview")).is_err(), "an empty list approves nothing");
    }

    #[test]
    fn the_override_names_libraries() {
        assert_eq!(dev_names("test, a,,b c"), ["test", "a", "b", "c"]);
        let (g, note) = default_gate(Some("test"));
        if cfg!(debug_assertions) {
            let a = g.check_library(&cand("test")).unwrap();
            assert_eq!((a.id.as_str(), a.data_name.as_str()), ("manual:test", "test"));
            assert!(note.unwrap().contains("debug build only"));
        } else {
            assert!(g.check_library(&cand("test")).is_err());
            assert!(note.unwrap().contains("ignored"));
        }
        assert!(g.check_library(&cand("other")).is_err(), "only the named ones");
    }

    #[test]
    fn release_builds_ignore_the_override() {
        let (g, note) = with_dev_override(Box::new(DenyUnapproved), Some("test"), false);
        assert!(g.check_library(&cand("test")).is_err());
        assert!(note.unwrap().contains("ignored in release builds"));
        let (g, note) = with_dev_override(Box::new(DenyUnapproved), Some("test"), true);
        assert!(g.check_library(&cand("test")).is_ok());
        assert!(note.unwrap().contains("debug build only"));
    }

    #[test]
    fn files_lists_stay_inside() {
        let l = files_list("# helpers\nhelper\n\n sub/tool.exe \n../escape\n/abs/path\n./dot\n");
        assert_eq!(l, [PathBuf::from("helper"), PathBuf::from("sub/tool.exe")]);
    }

    #[test]
    fn spawns_need_a_listed_file() {
        let dir = std::env::temp_dir().join(format!("tsext-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lib = dir.join("tsmod_test_amd64.so");
        std::fs::write(&lib, b"x").unwrap();
        std::fs::write(dir.join("helper"), b"x").unwrap();
        std::fs::write(dir.join("other"), b"x").unwrap();
        std::fs::write(dir.join("tsmod_test.files"), "helper\n").unwrap();
        let g = DevOverride { names: vec!["test".into()], inner: Box::new(DenyUnapproved) };
        let a = g.check_library(&Candidate { name: "test".into(), file: String::new(), library: lib.clone() }).unwrap();
        let ok = g.check_spawn(&a, &lib, &dir.join("helper"));
        let other = g.check_spawn(&a, &lib, &dir.join("other"));
        let missing = g.check_spawn(&a, &lib, &dir.join("missing"));
        let stranger = Approved { id: "webview".into(), data_name: "webview".into(), source: String::new() };
        let theirs = g.check_spawn(&stranger, &lib, &dir.join("helper"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(ok, Ok(()));
        assert!(other.unwrap_err().contains("isn't listed"));
        assert!(missing.is_err());
        assert!(theirs.is_err(), "a package's spawns go to the inner gate");
    }
}
