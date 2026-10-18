//! `cl_plugins/approved.toml`, the approval store (client-plugins.md §5.1).
//!
//! Only the approval dialogs and the launcher write it; nothing a server sends reaches it.
//! Writes are atomic (a temporary file, then a rename). A store that doesn't parse is set
//! aside as `approved.toml.bad` and treated as empty, so every plugin is prompted for again.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::hash::Sha256;
use crate::manifest::is_valid_id;
use crate::path::check_path_set;
use crate::platform::Platform;
use crate::sign::PublicKey;
use crate::trust::Trust;

/// The store format this build reads and writes.
pub const STORE_FORMAT: u32 = 1;
/// The store's file name in `cl_plugins/`.
pub const STORE_FILE: &str = "approved.toml";
/// The prefix of hand-installed libraries' ids (§5.3).
pub const MANUAL_PREFIX: &str = "manual:";

/// The approval store.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Store {
    format: u32,
    /// Every entry, in file order.
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<Entry>,
}

/// `state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Approved: loads while every hash matches.
    Allowed,
    /// "Never": not offered or prompted for again.
    Blocked,
}

/// `scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Everywhere.
    Always,
    /// Only on the addresses in `servers` (§7.2 "Allow on this server only", "Never from this
    /// server").
    Server,
}

/// One `[[plugin]]` entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The plugin id, or `manual:<name>` for a hand-installed library.
    pub id: String,
    /// The approved version (`0.0.0` for hand-installed libraries).
    pub version: Version,
    /// Allowed or blocked.
    pub state: State,
    /// Everywhere, or only on `servers`.
    pub scope: Scope,
    /// The `host:port` addresses for `scope = "server"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<String>,
    /// Where the approval came from: `server <address>`, `launcher` or `manual`.
    pub source: String,
    /// The publisher key it was approved under; absent for unsigned plugins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_key: Option<PublicKey>,
    /// The trust level shown when it was approved (§3.3).
    pub trust: Trust,
    /// RFC 3339, UTC.
    pub approved_at: String,
    /// SHA-256 of `manifest.toml`; absent for hand-installed libraries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<Sha256>,
    /// The client platform the files are for.
    pub platform: Platform,
    /// Every approved file and its hash. For a package, paths are relative to its
    /// `cl_plugins/<id>/<version>/` directory (`manifest.toml`, `files/...`); for a
    /// hand-installed library, relative to `cl_dlls/`.
    #[serde(default)]
    pub files: BTreeMap<String, Sha256>,
}

impl Entry {
    /// Whether this is a hand-installed library's entry.
    pub fn is_manual(&self) -> bool {
        self.id.starts_with(MANUAL_PREFIX)
    }

    /// Whether the entry applies on `server` (`None`: not connected).
    pub fn applies_to(&self, server: Option<&str>) -> bool {
        match self.scope {
            Scope::Always => true,
            Scope::Server => server.is_some_and(|s| self.servers.iter().any(|a| a == s)),
        }
    }

    /// Re-hashes one approved file just before it's used, for `verified_spawn` (§5.2).
    /// `base` is the directory the entry's paths are relative to. Refuses a file that isn't
    /// in the approval, isn't a regular file, or whose hash has changed.
    pub fn check_file(&self, base: &Path, rel: &str) -> Result<(), String> {
        if self.state != State::Allowed {
            return Err(format!("{} isn't approved", self.id));
        }
        let expected = self.files.get(rel).ok_or_else(|| format!("{rel} isn't in {}'s approval", self.id))?;
        match Sha256::of_file(&base.join(rel)) {
            Ok((h, _)) if h == *expected => Ok(()),
            Ok(_) => Err(format!("{rel} changed since it was approved")),
            Err(e) => Err(format!("{rel}: {e}")),
        }
    }
}

/// Why a store's text is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError(pub String);

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "approval store: {}", self.0)
    }
}

impl std::error::Error for StoreError {}

/// What [`Store::load`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadStatus {
    /// No store yet: nothing is approved.
    Missing,
    /// The store parsed.
    Loaded,
    /// The store didn't parse. It was renamed to `bad`, and the store is empty.
    SetAside {
        /// Where the old store is now.
        bad: PathBuf,
        /// Why it was refused.
        error: String,
    },
}

/// `id` is a manifest id, or `manual:<name>` with a valid name.
fn is_valid_store_id(id: &str) -> bool {
    is_valid_id(id.strip_prefix(MANUAL_PREFIX).unwrap_or(id))
}

impl Store {
    /// An empty store.
    pub fn new() -> Store {
        Store { format: STORE_FORMAT, plugins: Vec::new() }
    }

    /// Parses and validates a store's text.
    pub fn parse(text: &str) -> Result<Store, StoreError> {
        let store: Store = toml::from_str(text).map_err(|e| StoreError(e.message().to_string()))?;
        if store.format != STORE_FORMAT {
            return Err(StoreError(format!("format {} (this build reads {STORE_FORMAT})", store.format)));
        }
        for e in &store.plugins {
            if !is_valid_store_id(&e.id) {
                return Err(StoreError(format!("bad plugin id {:?}", e.id)));
            }
            check_path_set(e.files.keys().map(String::as_str)).map_err(|err| StoreError(format!("{}: {err}", e.id)))?;
        }
        Ok(store)
    }

    /// The store as TOML.
    pub fn to_toml(&self) -> String {
        let mut text = String::from("# The Specialists port: approved client plugins. Written by the game's approval dialogs\n");
        text.push_str("# and tsmod-launcher. Every file is hashed at every start; any change asks again.\n");
        text.push_str(&toml::to_string(&Store { format: STORE_FORMAT, plugins: self.plugins.clone() }).unwrap_or_default());
        text
    }

    /// Reads the store at `path`. A missing store is empty. One that doesn't parse is renamed
    /// to `<path>.bad` (replacing an older one) and the store is empty.
    pub fn load(path: &Path) -> io::Result<(Store, LoadStatus)> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((Store::new(), LoadStatus::Missing)),
            Err(e) => return Err(e),
        };
        let parsed = std::str::from_utf8(&bytes).map_err(|_| StoreError("not UTF-8".into())).and_then(Store::parse);
        match parsed {
            Ok(store) => Ok((store, LoadStatus::Loaded)),
            Err(e) => {
                let bad = sibling(path, ".bad");
                fs::rename(path, &bad)?;
                Ok((Store::new(), LoadStatus::SetAside { bad, error: e.0 }))
            }
        }
    }

    /// Writes the store to `path` atomically: a temporary file next to it, flushed, then
    /// renamed over it. Creates the directory if needed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let tmp = sibling(path, ".tmp");
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(self.to_toml().as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// The entry for exactly this id, version and platform.
    pub fn find(&self, id: &str, version: &Version, platform: Platform) -> Option<&Entry> {
        self.plugins.iter().find(|e| e.id == id && e.version == *version && e.platform == platform)
    }

    /// Every entry for `id`.
    pub fn entries_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a Entry> {
        self.plugins.iter().filter(move |e| e.id == id)
    }

    /// Records an approval. Replaces every other entry for the same id and platform, blocked
    /// ones included: one version is active at a time (§4).
    pub fn allow(&mut self, entry: Entry) {
        self.plugins.retain(|e| !(e.id == entry.id && e.platform == entry.platform));
        self.plugins.push(entry);
    }

    /// Records a "Never": `id` under `publisher_key` isn't offered or prompted for again,
    /// everywhere (`servers` empty) or on those servers. Replaces the id's allowed entries for
    /// that platform when it's everywhere.
    pub fn block(&mut self, mut entry: Entry) {
        entry.state = State::Blocked;
        entry.files.clear();
        if entry.servers.is_empty() {
            entry.scope = Scope::Always;
            self.plugins.retain(|e| !(e.id == entry.id && e.platform == entry.platform));
        } else {
            entry.scope = Scope::Server;
        }
        self.plugins.push(entry);
    }

    /// Removes every entry for `id` (revoke, or unblock). Returns how many went.
    pub fn remove(&mut self, id: &str) -> usize {
        let before = self.plugins.len();
        self.plugins.retain(|e| e.id != id);
        before - self.plugins.len()
    }

    /// Removes the entries for `id` in `state` only: revoking keeps a "Never", and unblocking
    /// keeps an approval (the client's `cl_plugins_menu`, §8). Returns how many went.
    pub fn remove_state(&mut self, id: &str, state: State) -> usize {
        let before = self.plugins.len();
        self.plugins.retain(|e| !(e.id == id && e.state == state));
        before - self.plugins.len()
    }

    /// Whether `id` from `publisher_key` is blocked, everywhere or on `server`. A block is for
    /// one publisher key: the same id from another key counts as another plugin (§3.3).
    pub fn is_blocked(&self, id: &str, publisher_key: Option<PublicKey>, server: Option<&str>) -> bool {
        self.entries_for(id).any(|e| e.state == State::Blocked && e.publisher_key == publisher_key && e.applies_to(server))
    }

    /// The earliest allowed entry under `key`: the player has approved this publisher before.
    pub fn known_publisher(&self, key: &PublicKey) -> Option<&Entry> {
        self.plugins
            .iter()
            .filter(|e| e.state == State::Allowed && e.publisher_key.as_ref() == Some(key))
            .min_by(|a, b| a.approved_at.cmp(&b.approved_at))
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| STORE_FILE.into());
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::key;

    pub(crate) fn entry(id: &str, version: &str) -> Entry {
        Entry {
            id: id.into(),
            version: version.parse().unwrap(),
            state: State::Allowed,
            scope: Scope::Always,
            servers: vec![],
            source: "manual".into(),
            publisher_key: None,
            trust: Trust::Unsigned,
            approved_at: "2026-10-17T10:42:00Z".into(),
            manifest_sha256: Some(Sha256::of_bytes(b"m")),
            platform: Platform::LinuxX86_64,
            files: BTreeMap::from([
                ("manifest.toml".to_string(), Sha256::of_bytes(b"m")),
                ("files/lib/linux-x86_64/tsmod_webview.so".to_string(), Sha256::of_bytes(b"so")),
            ]),
        }
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cl_plugins").join(STORE_FILE);
        let mut store = Store::new();
        let mut a = entry("webview", "0.1.0");
        a.publisher_key = Some(PublicKey::of(&key(1)));
        a.trust = Trust::Verified;
        a.scope = Scope::Server;
        a.servers = vec!["203.0.113.5:27015".into()];
        a.source = "server 203.0.113.5:27015".into();
        store.allow(a);
        let mut m = entry("manual:dev", "0.0.0");
        m.manifest_sha256 = None;
        m.files = BTreeMap::from([("tsmod_dev.so".to_string(), Sha256::of_bytes(b"dev"))]);
        store.allow(m);
        store.save(&path).unwrap();
        assert!(!sibling(&path, ".tmp").exists());

        let (loaded, status) = Store::load(&path).unwrap();
        assert_eq!(status, LoadStatus::Loaded);
        assert_eq!(loaded, store);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[plugin]]") && text.contains("[plugin.files]"), "{text}");
        assert!(text.contains("state = \"allowed\"") && text.contains("trust = \"verified\""), "{text}");

        // Saving again replaces the file.
        store.remove("manual:dev");
        store.save(&path).unwrap();
        assert_eq!(Store::load(&path).unwrap().0.plugins.len(), 1);
    }

    #[test]
    fn missing_and_bad_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STORE_FILE);
        assert_eq!(Store::load(&path).unwrap(), (Store::new(), LoadStatus::Missing));

        for bad in [
            "this isn't toml [".as_bytes(),
            b"format = 2\n",
            b"\xff\xfe",
            b"format = 1\n[[plugin]]\nid = \"x\"\n",
            b"format = 1\nsurprise = true\n",
        ] {
            fs::write(&path, bad).unwrap();
            let (store, status) = Store::load(&path).unwrap();
            assert_eq!(store, Store::new());
            let LoadStatus::SetAside { bad: moved, .. } = status else { panic!("{status:?}") };
            assert_eq!(moved, dir.path().join("approved.toml.bad"));
            assert_eq!(fs::read(&moved).unwrap(), bad);
            assert!(!path.exists());
        }

        // A store whose file paths break the path rules is bad too: verification would
        // otherwise open them.
        let mut s = Store::new();
        let mut e = entry("webview", "0.1.0");
        e.files.insert("../../etc/passwd".into(), Sha256::of_bytes(b""));
        s.plugins.push(e);
        fs::write(&path, s.to_toml()).unwrap();
        assert!(matches!(Store::load(&path).unwrap().1, LoadStatus::SetAside { .. }));
        let mut s = Store::new();
        s.plugins.push(entry("Bad Id", "0.1.0"));
        assert!(Store::parse(&s.to_toml()).is_err());
    }

    #[test]
    fn allow_replaces_and_block_scopes() {
        let k = Some(PublicKey::of(&key(1)));
        let mut store = Store::new();
        store.allow(entry("webview", "0.1.0"));
        store.allow(entry("webview", "0.2.0"));
        let mut win = entry("webview", "0.1.0");
        win.platform = Platform::WindowsI686;
        store.allow(win);
        assert_eq!(store.entries_for("webview").count(), 2, "one version per platform");
        assert!(store.find("webview", &"0.2.0".parse().unwrap(), Platform::LinuxX86_64).is_some());
        assert!(store.find("webview", &"0.1.0".parse().unwrap(), Platform::LinuxX86_64).is_none());

        let mut b = entry("ads", "1.0.0");
        b.publisher_key = k;
        b.servers = vec!["10.0.0.1:27015".into()];
        store.block(b.clone());
        assert!(store.is_blocked("ads", k, Some("10.0.0.1:27015")));
        assert!(!store.is_blocked("ads", k, Some("10.0.0.2:27015")));
        assert!(!store.is_blocked("ads", k, None));
        assert!(!store.is_blocked("ads", None, Some("10.0.0.1:27015")), "another key is another plugin");
        b.servers.clear();
        store.block(b);
        assert!(store.is_blocked("ads", k, None));
        assert!(store.is_blocked("ads", k, Some("10.0.0.2:27015")));
        assert_eq!(store.remove("ads"), 1, "the block everywhere replaced the per-server one");
        assert!(!store.is_blocked("ads", k, None));
    }

    #[test]
    fn remove_state_keeps_the_other_state() {
        let mut store = Store::new();
        store.allow(entry("web", "0.1.0"));
        let mut b = entry("web", "0.1.0");
        b.servers = vec!["10.0.0.1:27015".into()];
        store.block(b);
        store.allow(entry("other", "1.0.0"));
        assert_eq!(store.remove_state("web", State::Blocked), 1);
        assert_eq!(store.entries_for("web").map(|e| e.state).collect::<Vec<_>>(), [State::Allowed]);
        assert_eq!(store.remove_state("web", State::Blocked), 0);
        assert_eq!(store.remove_state("web", State::Allowed), 1);
        assert_eq!(store.plugins.len(), 1, "other ids stay");
    }

    #[test]
    fn known_publisher_is_the_earliest_allowed() {
        let k = PublicKey::of(&key(1));
        let mut store = Store::new();
        let mut a = entry("a", "1.0.0");
        a.publisher_key = Some(k);
        a.approved_at = "2026-10-18T00:00:00Z".into();
        let mut b = entry("b", "1.0.0");
        b.publisher_key = Some(k);
        b.approved_at = "2026-01-01T00:00:00Z".into();
        let mut c = entry("c", "1.0.0");
        c.publisher_key = Some(k);
        c.approved_at = "2025-01-01T00:00:00Z".into();
        store.allow(a);
        store.allow(b);
        store.block(c);
        assert_eq!(store.known_publisher(&k).unwrap().id, "b", "blocked entries don't count");
        assert!(store.known_publisher(&PublicKey::of(&key(2))).is_none());
    }

    #[test]
    fn check_file_rehashes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("bin")).unwrap();
        fs::write(dir.path().join("bin/host"), b"host").unwrap();
        let mut e = entry("webview", "0.1.0");
        e.files = BTreeMap::from([("bin/host".to_string(), Sha256::of_bytes(b"host"))]);
        assert_eq!(e.check_file(dir.path(), "bin/host"), Ok(()));
        assert!(e.check_file(dir.path(), "bin/other").is_err());
        fs::write(dir.path().join("bin/host"), b"evil").unwrap();
        assert!(e.check_file(dir.path(), "bin/host").is_err());
        fs::remove_file(dir.path().join("bin/host")).unwrap();
        assert!(e.check_file(dir.path(), "bin/host").is_err());
    }
}
