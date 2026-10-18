//! The client's side of a server's ops 3 `require` and 4 `block` (client-plugins.md §13.3, issue
//! #302): engine-free decisions, unit-tested; `super` suspends, answers and queues cards.
//!
//! * **Op 4 with `suspend`:** every loaded plugin the block names (its id, or `manual:<name>` /
//!   `manual:*`, at a version the requirement accepts) is suspended through
//!   `ext::suspend` until the player leaves the server, and answered `suspended`.
//! * **Op 4 with `kick`:** printed; the server drops the player.
//! * **Op 3:** a loaded version the requirement accepts needs nothing. Otherwise, when a version
//!   that does is installed and approved here (the player chose "Later" on an update's restart),
//!   the client answers `restart` and offers the restart card, which restarts through
//!   `relaunch::restart`; when none is, the server's offer brings the update card.
//!
//! A server can make the client suspend a plugin or leave; nothing here approves, installs or
//! loads anything (§7.3).

use plugin_pkg::{Platform, State, Store, Version, VersionReq};
use std::path::Path;

/// A loaded plugin as the client reports it in `tspkg list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedPlugin {
    /// The plugin id, or `manual:<name>` for a hand-installed library.
    pub id: String,
    pub version: String,
    /// Its extension's name (`ext::suspend` takes it).
    pub ext: String,
}

/// Whether a block's `pattern` (a plugin id, `manual:<name>` or `manual:*`) names `id`.
pub fn names(pattern: &str, id: &str) -> bool {
    match pattern.strip_prefix(plugin_pkg::store::MANUAL_PREFIX) {
        Some("*") => id.starts_with(plugin_pkg::store::MANUAL_PREFIX),
        _ => pattern == id,
    }
}

/// A server's requirement; `Err` (for the console) when it doesn't parse.
pub fn requirement(s: &str) -> Result<VersionReq, String> {
    VersionReq::parse(s.trim()).map_err(|e| format!("requirement {s:?}: {e}"))
}

fn version(s: &str) -> Version {
    Version::parse(s).unwrap_or(Version::new(0, 0, 0))
}

/// The loaded plugins an op 4 names.
pub fn blocked<'a>(loaded: &'a [LoadedPlugin], pattern: &str, req: &VersionReq) -> Vec<&'a LoadedPlugin> {
    loaded.iter().filter(|p| names(pattern, &p.id) && req.matches(&version(&p.version))).collect()
}

/// What op 3 asks of the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Require {
    /// Not loaded, or loaded at a version the requirement accepts.
    Nothing,
    /// Loaded at another version, and `0` is installed and approved for this server: restart.
    Restart(Version),
    /// Loaded at another version, and none that fits is installed: the server's offer updates it.
    AwaitOffer { loaded: String },
}

/// Op 3 for `id`: what the client has loaded, and what the store has installed and approved for
/// `server` on `platform` (in `cl_plugins`).
pub fn require(
    loaded: &[LoadedPlugin],
    id: &str,
    req: &VersionReq,
    store: &Store,
    platform: Platform,
    server: &str,
    cl_plugins: &Path,
) -> Require {
    let Some(p) = loaded.iter().find(|p| p.id == id) else { return Require::Nothing };
    if req.matches(&version(&p.version)) {
        return Require::Nothing;
    }
    let installed = store.entries_for(id).find(|e| {
        e.state == State::Allowed
            && e.platform == platform
            && e.applies_to(Some(server))
            && req.matches(&e.version)
            && e.version.to_string() != p.version
            && super::flow::version_dir(cl_plugins, id, &e.version).is_dir()
    });
    match installed {
        Some(e) => Require::Restart(e.version.clone()),
        None => Require::AwaitOffer { loaded: p.version.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(id: &str, v: &str) -> LoadedPlugin {
        LoadedPlugin { id: id.into(), version: v.into(), ext: id.trim_start_matches("manual:").into() }
    }

    #[test]
    fn blocks_name_plugins() {
        let loaded = [lp("webview", "0.1.0"), lp("manual:dev", "0.0.0"), lp("stats", "1.0.0")];
        let star = requirement("*").unwrap();
        assert_eq!(blocked(&loaded, "webview", &requirement("<0.2.0").unwrap()), [&loaded[0]]);
        assert!(blocked(&loaded, "webview", &requirement(">=0.2.0").unwrap()).is_empty());
        assert_eq!(blocked(&loaded, "manual:*", &star), [&loaded[1]]);
        assert_eq!(blocked(&loaded, "manual:dev", &star), [&loaded[1]]);
        assert!(blocked(&loaded, "dev", &star).is_empty(), "a plugin id never names a hand-installed library");
        assert!(blocked(&loaded, "manual:webview", &star).is_empty());
        assert!(requirement("newest").is_err());
    }

    #[test]
    fn require_restarts_into_an_installed_version() {
        let dir = tempfile::tempdir().unwrap();
        let cl = dir.path();
        let p = Platform::LinuxX86_64;
        let entry = |v: &str, scope: plugin_pkg::Scope, servers: Vec<String>| plugin_pkg::Entry {
            id: "web".into(),
            version: v.parse().unwrap(),
            state: State::Allowed,
            scope,
            servers,
            source: "manual".into(),
            publisher_key: None,
            trust: plugin_pkg::Trust::Unsigned,
            approved_at: "2026-10-18T10:00:00Z".into(),
            manifest_sha256: None,
            platform: p,
            files: Default::default(),
        };
        let loaded = [lp("web", "0.1.0")];
        let req = requirement("=0.2.0").unwrap();
        let mut store = Store::new();
        assert_eq!(require(&loaded, "web", &req, &store, p, "1.2.3.4:27015", cl), Require::AwaitOffer { loaded: "0.1.0".into() });
        assert_eq!(require(&loaded, "other", &req, &store, p, "1.2.3.4:27015", cl), Require::Nothing, "not loaded");
        assert_eq!(require(&loaded, "web", &requirement("^0.1").unwrap(), &store, p, "s", cl), Require::Nothing, "accepted");
        store.allow(entry("0.2.0", plugin_pkg::Scope::Always, vec![]));
        assert_eq!(require(&loaded, "web", &req, &store, p, "s", cl), Require::AwaitOffer { loaded: "0.1.0".into() }, "not installed");
        std::fs::create_dir_all(cl.join("web/0.2.0")).unwrap();
        assert_eq!(require(&loaded, "web", &req, &store, p, "s", cl), Require::Restart("0.2.0".parse().unwrap()));
        assert!(matches!(require(&loaded, "web", &req, &store, Platform::WindowsI686, "s", cl), Require::AwaitOffer { .. }));
        store.allow(entry("0.2.0", plugin_pkg::Scope::Server, vec!["other:27015".into()]));
        assert!(matches!(require(&loaded, "web", &req, &store, p, "s", cl), Require::AwaitOffer { .. }), "approved for another server");
    }
}
