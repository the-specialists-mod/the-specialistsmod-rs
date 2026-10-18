//! What the plugins panel lists and what its buttons do to the store and the disk
//! (client-plugins.md §8, §9): engine-free, so the unit tests run it on temporary directories.
//!
//! A [`Row`] is a plugin start-up found (`cl_plugins/<id>/<version>/` or a hand-installed
//! library), with every store entry for its id, or an id the store knows that wasn't found
//! (a "Never", or an approval whose files are gone). [`apply`] revokes, unblocks or deletes.

use super::super::startup::Item;
use plugin_pkg::{Candidate, Entry, Platform, Sha256, State, Store};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// How a file found at start compares with the approval for its version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// No approval to compare with (or the plugin isn't installed: the approved hash).
    Plain,
    /// The approved hash.
    Matches,
    /// Another hash than the approved one.
    Differs,
    /// Not in the approval.
    Extra,
    /// Approved, but not found.
    Missing,
}

impl Mark {
    pub fn text(self) -> &'static str {
        match self {
            Mark::Plain | Mark::Matches => "",
            Mark::Differs => " (changed since it was approved)",
            Mark::Extra => " (not in its approval)",
            Mark::Missing => " (approved, missing)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    pub hash: Sha256,
    pub mark: Mark,
}

/// One plugin in the panel.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    /// The manifest's name, a library's file name, or the id.
    pub name: String,
    /// The version found (or, when not installed, the store's).
    pub version: Option<String>,
    /// Every store entry for the id.
    pub entries: Vec<Entry>,
    /// `cl_plugins/<id>/<version>/`, for a package found at start.
    pub dir: Option<PathBuf>,
    /// A hand-installed library (`cl_dlls/`).
    pub manual: bool,
    /// Found at start.
    pub installed: bool,
    /// Its extension is loaded now.
    pub loaded: bool,
    /// What start-up decided, or that it's loaded.
    pub status: String,
    pub files: Vec<FileRow>,
}

impl Row {
    pub fn allowed(&self) -> bool {
        self.entries.iter().any(|e| e.state == State::Allowed)
    }
    pub fn blocked(&self) -> bool {
        self.entries.iter().any(|e| e.state == State::Blocked)
    }
    /// "<name> <version>", or the name alone.
    pub fn title(&self) -> String {
        match &self.version {
            Some(v) if !self.manual => format!("{} {v}", self.name),
            _ => self.name.clone(),
        }
    }
    /// What the list shows for it: its approval, and whether it's loaded.
    pub fn summary(&self) -> String {
        let approval = if self.entries.is_empty() {
            "not approved".to_string()
        } else {
            let mut kinds: Vec<&str> = Vec::new();
            if self.allowed() {
                kinds.push("allowed");
            }
            if self.blocked() {
                kinds.push("blocked (\"Never\")");
            }
            kinds.join(", ")
        };
        let status = if self.installed { self.status.clone() } else { "not installed".into() };
        format!("{} ({}): {approval}; {status}", self.title(), self.id)
    }
}

/// The panel's rows: every plugin found at start (`items`, with `status(item)` its loaded state
/// and `is_loaded(item)` whether it's loaded), then every other id the store knows, by id.
pub fn rows(
    items: &[Item],
    store: &Store,
    platform: Platform,
    status: &dyn Fn(&Item) -> String,
    is_loaded: &dyn Fn(&Item) -> bool,
) -> Vec<Row> {
    let mut out: Vec<Row> = items
        .iter()
        .map(|it| {
            let id = it.candidate.id();
            let entries: Vec<Entry> = store.entries_for(&id).cloned().collect();
            let version = it.found.version.clone();
            let approved = version.as_ref().and_then(|v| store.find(&id, v, platform)).filter(|e| e.state == State::Allowed);
            let (dir, manual) = match &it.candidate {
                Candidate::Package { dir } => (Some(dir.clone()), false),
                Candidate::Manual { .. } => (None, true),
            };
            Row {
                name: if it.found.name.is_empty() { id.clone() } else { it.found.name.clone() },
                version: version.map(|v| v.to_string()),
                entries,
                dir,
                manual,
                installed: true,
                loaded: is_loaded(it),
                status: status(it),
                files: compare(&it.found.files, approved.map(|e| &e.files)),
                id,
            }
        })
        .collect();
    let found: BTreeSet<String> = out.iter().map(|r| r.id.clone()).collect();
    let others: BTreeSet<&str> = store.plugins.iter().map(|e| e.id.as_str()).filter(|id| !found.contains(*id)).collect();
    for id in others {
        let entries: Vec<Entry> = store.entries_for(id).cloned().collect();
        let shown = entries.iter().find(|e| e.state == State::Allowed && e.platform == platform).or(entries.first());
        let files = shown
            .map(|e| e.files.iter().map(|(p, h)| FileRow { path: p.clone(), hash: *h, mark: Mark::Plain }).collect())
            .unwrap_or_default();
        out.push(Row {
            id: id.to_owned(),
            name: id.to_owned(),
            version: shown.map(|e| e.version.to_string()),
            dir: None,
            manual: id.starts_with(plugin_pkg::store::MANUAL_PREFIX),
            installed: false,
            loaded: false,
            status: String::new(),
            files,
            entries,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.version.cmp(&b.version)));
    out
}

/// Every file found, marked against the approval's hashes, then the approved files not found.
pub fn compare(
    found: &std::collections::BTreeMap<String, Sha256>,
    approved: Option<&std::collections::BTreeMap<String, Sha256>>,
) -> Vec<FileRow> {
    let mut out: Vec<FileRow> = found
        .iter()
        .map(|(p, h)| {
            let mark = match approved.map(|a| a.get(p)) {
                None => Mark::Plain,
                Some(None) => Mark::Extra,
                Some(Some(a)) if a == h => Mark::Matches,
                Some(Some(_)) => Mark::Differs,
            };
            FileRow { path: p.clone(), hash: *h, mark }
        })
        .collect();
    for (p, h) in approved.into_iter().flatten() {
        if !found.contains_key(p) {
            out.push(FileRow { path: p.clone(), hash: *h, mark: Mark::Missing });
        }
    }
    out
}

/// What a panel button does (behind its confirmation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Removes the approval (`state = "allowed"` entries): it doesn't load from the next start
    /// (a loaded plugin stays loaded until the game exits, §9).
    Revoke,
    /// Removes the "Never" (`state = "blocked"` entries): asked about again, offered again.
    Unblock,
    /// Removes `cl_plugins/<id>/<version>/` and that version's approval; not while it's loaded.
    Delete,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::Revoke => "Revoke approval",
            Op::Unblock => "Unblock",
            Op::Delete => "Delete files",
        }
    }
    /// The e2e hook's word (`menu:<id>=<word>`).
    pub fn name(self) -> &'static str {
        match self {
            Op::Revoke => "revoke",
            Op::Unblock => "unblock",
            Op::Delete => "delete",
        }
    }
    pub fn parse(s: &str) -> Option<Op> {
        [Op::Revoke, Op::Unblock, Op::Delete].into_iter().find(|o| o.name() == s.trim())
    }
    /// Whether the row's details offer it.
    pub fn offered(self, row: &Row) -> bool {
        match self {
            Op::Revoke => row.allowed(),
            Op::Unblock => row.blocked(),
            Op::Delete => row.dir.is_some() && !row.loaded,
        }
    }
    /// The confirmation's text.
    pub fn confirm_lines(self, row: &Row) -> Vec<String> {
        let what = format!("{} ({})", row.title(), row.id);
        match self {
            Op::Revoke => {
                let mut l = vec![format!("Revoke the approval of {what}?"), "It won't load from the next start.".into()];
                if row.loaded {
                    l.push("It's loaded now, and stays loaded until the game exits.".into());
                }
                if row.installed {
                    l.push("While it's installed, the next start asks about it again.".into());
                }
                l
            }
            Op::Unblock => vec![
                format!("Unblock {what}?"),
                "Your \"Never\" goes: the next start asks about it if it's installed, and servers can offer it again.".into(),
            ],
            Op::Delete => {
                let dir = row.dir.as_ref().map_or_else(String::new, |d| shown_dir(d));
                vec![
                    format!("Delete {what}?"),
                    format!("This removes {dir} ({} file(s)) and the approval of this version.", row.files.len()),
                    format!("A \"Never\" stays, and so does its own data in cl_plugins_data/{}/.", row.id),
                ]
            }
        }
    }
}

/// `cl_plugins/<id>/<version>` for people.
pub fn shown_dir(dir: &Path) -> String {
    let parts: Vec<String> = dir.iter().rev().take(3).map(|p| p.to_string_lossy().into_owned()).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

/// A package directory the panel may delete: `<cl_plugins>/<id>/<version>`, a real directory
/// (not a link), not hidden and not `staging`.
fn deletable(cl_plugins: &Path, dir: &Path, id: &str) -> Result<(), String> {
    let version = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent_ok = dir.parent().is_some_and(|p| p == cl_plugins.join(id));
    if !parent_ok || version.is_empty() || version.starts_with('.') || id == plugin_pkg::verify::STAGING_DIR || id.starts_with('.') {
        return Err(format!("{} isn't a plugin's directory", dir.display()));
    }
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} isn't a directory", dir.display())),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// What [`apply`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// For the panel and the console.
    pub message: String,
    /// The store changed and must be saved.
    pub store_changed: bool,
}

/// Does `op` to `row`: the store (in memory; the caller saves it) and, for a delete, the disk.
pub fn apply(op: Op, row: &Row, store: &mut Store, cl_plugins: &Path) -> Result<Applied, String> {
    let what = format!("{} ({})", row.title(), row.id);
    match op {
        Op::Revoke => {
            if store.remove_state(&row.id, State::Allowed) == 0 {
                return Err(format!("{what} has no approval to revoke"));
            }
            let loaded = if row.loaded { "; it stays loaded until the game exits" } else { "" };
            Ok(Applied { message: format!("revoked {what}: it won't load from the next start{loaded}"), store_changed: true })
        }
        Op::Unblock => {
            if store.remove_state(&row.id, State::Blocked) == 0 {
                return Err(format!("{what} isn't blocked"));
            }
            Ok(Applied { message: format!("unblocked {what}: it can be asked about and offered again"), store_changed: true })
        }
        Op::Delete => {
            if row.loaded {
                return Err(format!("{what} is loaded: revoke it, and delete it after the next start"));
            }
            let dir = row.dir.as_ref().ok_or_else(|| format!("{what} has no package directory to delete"))?;
            deletable(cl_plugins, dir, &row.id)?;
            std::fs::remove_dir_all(dir).map_err(|e| format!("can't delete {}: {e}", dir.display()))?;
            // The id's directory goes with its last version.
            if let Some(id_dir) = dir.parent() {
                let _ = std::fs::remove_dir(id_dir);
            }
            let before = store.plugins.len();
            if let Some(v) = row.version.as_ref().and_then(|v| v.parse::<plugin_pkg::Version>().ok()) {
                store.plugins.retain(|e| !(e.id == row.id && e.state == State::Allowed && e.version == v));
            }
            let store_changed = store.plugins.len() != before;
            Ok(Applied { message: format!("deleted {what} ({})", shown_dir(dir)), store_changed })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::startup::{Decision, Found};
    use super::*;
    use plugin_pkg::{Scope, Trust};
    use std::collections::BTreeMap;

    const P: Platform = Platform::LinuxX86_64;

    fn entry(id: &str, version: &str, state: State, files: &[(&str, &[u8])]) -> Entry {
        Entry {
            id: id.into(),
            version: version.parse().unwrap(),
            state,
            scope: Scope::Always,
            servers: vec![],
            source: "manual".into(),
            publisher_key: None,
            trust: Trust::Unsigned,
            approved_at: "2026-10-18T10:00:00Z".into(),
            manifest_sha256: None,
            platform: P,
            files: files.iter().map(|(p, b)| (p.to_string(), Sha256::of_bytes(b))).collect(),
        }
    }

    fn item(dir: &Path, name: &str, version: &str, files: &[(&str, &[u8])]) -> Item {
        Item {
            candidate: Candidate::Package { dir: dir.to_path_buf() },
            name: name.into(),
            library: None,
            decision: Decision::Skip("x".into()),
            found: Found {
                name: format!("{name} plugin"),
                version: Some(version.parse().unwrap()),
                files: files.iter().map(|(p, b)| (p.to_string(), Sha256::of_bytes(b))).collect(),
            },
        }
    }

    fn installed(cl: &Path, id: &str, version: &str) -> PathBuf {
        let dir = cl.join(id).join(version);
        std::fs::create_dir_all(dir.join("files")).unwrap();
        std::fs::write(dir.join("manifest.toml"), b"m").unwrap();
        std::fs::write(dir.join("files/lib.so"), b"so").unwrap();
        dir
    }

    fn the_rows(items: &[Item], store: &Store, loaded: &[&str]) -> Vec<Row> {
        rows(items, store, P, &|it| format!("status of {}", it.name), &|it| loaded.contains(&it.name.as_str()))
    }

    #[test]
    fn rows_join_what_start_found_with_the_store() {
        let cl = Path::new("/g/cl_plugins");
        let mut store = Store::new();
        store.allow(entry("web", "0.2.0", State::Allowed, &[("manifest.toml", b"m"), ("files/lib.so", b"old"), ("files/gone", b"g")]));
        let mut never = entry("ads", "1.0.0", State::Blocked, &[]);
        never.servers = vec!["1.2.3.4:27015".into()];
        store.block(never);
        let items = [item(&cl.join("web/0.2.0"), "web", "0.2.0", &[("manifest.toml", b"m"), ("files/lib.so", b"so"), ("files/new", b"n")])];
        let r = the_rows(&items, &store, &["web"]);
        assert_eq!(r.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["ads", "web"], "by id, the store's other ids too");
        let ads = &r[0];
        assert!(!ads.installed && ads.blocked() && !ads.allowed() && ads.files.is_empty());
        assert!(ads.summary().contains("blocked (\"Never\"); not installed"), "{}", ads.summary());
        let web = &r[1];
        assert!(web.installed && web.loaded && web.allowed() && web.dir.as_deref() == Some(cl.join("web/0.2.0").as_path()));
        assert_eq!(web.summary(), "web plugin 0.2.0 (web): allowed; status of web");
        let marks: BTreeMap<&str, Mark> = web.files.iter().map(|f| (f.path.as_str(), f.mark)).collect();
        assert_eq!(
            marks,
            BTreeMap::from([
                ("manifest.toml", Mark::Matches),
                ("files/lib.so", Mark::Differs),
                ("files/new", Mark::Extra),
                ("files/gone", Mark::Missing)
            ])
        );
        assert_eq!(web.files.iter().find(|f| f.path == "files/lib.so").unwrap().hash, Sha256::of_bytes(b"so"), "the hash found");
    }

    #[test]
    fn an_unapproved_plugin_lists_its_files_plainly() {
        let items = [item(Path::new("/g/cl_plugins/x/1.0.0"), "x", "1.0.0", &[("manifest.toml", b"m")])];
        let r = the_rows(&items, &Store::new(), &[]);
        assert_eq!(r[0].files, [FileRow { path: "manifest.toml".into(), hash: Sha256::of_bytes(b"m"), mark: Mark::Plain }]);
        assert!(r[0].summary().contains("not approved; status of x"));
        assert!(!Op::Revoke.offered(&r[0]) && !Op::Unblock.offered(&r[0]) && Op::Delete.offered(&r[0]));
    }

    #[test]
    fn revoke_and_unblock_touch_their_state_only() {
        let dir = tempfile::tempdir().unwrap();
        let cl = dir.path().join("cl_plugins");
        let mut store = Store::new();
        store.allow(entry("web", "0.2.0", State::Allowed, &[("manifest.toml", b"m")]));
        let mut never = entry("web", "0.2.0", State::Blocked, &[]);
        never.servers = vec!["1.2.3.4:27015".into()];
        store.block(never);
        let items = [item(&installed(&cl, "web", "0.2.0"), "web", "0.2.0", &[("manifest.toml", b"m")])];
        let row = the_rows(&items, &store, &["web"]).remove(0);
        assert!(Op::Revoke.offered(&row) && Op::Unblock.offered(&row) && !Op::Delete.offered(&row), "not delete while loaded");
        assert!(Op::Revoke.confirm_lines(&row).iter().any(|l| l.contains("stays loaded until the game exits")));

        let a = apply(Op::Revoke, &row, &mut store, &cl).unwrap();
        assert!(a.store_changed && a.message.contains("stays loaded"), "{}", a.message);
        assert_eq!(store.entries_for("web").map(|e| e.state).collect::<Vec<_>>(), [State::Blocked], "the \"Never\" stays");
        assert!(apply(Op::Revoke, &row, &mut store, &cl).is_err(), "nothing left to revoke");
        assert!(cl.join("web/0.2.0/manifest.toml").exists(), "revoking leaves the files");

        apply(Op::Unblock, &row, &mut store, &cl).unwrap();
        assert_eq!(store.entries_for("web").count(), 0);
        assert!(apply(Op::Unblock, &row, &mut store, &cl).is_err());
    }

    #[test]
    fn delete_removes_the_version_and_its_approval() {
        let dir = tempfile::tempdir().unwrap();
        let cl = dir.path().join("cl_plugins");
        let v1 = installed(&cl, "web", "0.1.0");
        let v2 = installed(&cl, "web", "0.2.0");
        let mut store = Store::new();
        store.allow(entry("web", "0.2.0", State::Allowed, &[("manifest.toml", b"m")]));
        let mut never = entry("web", "0.1.0", State::Blocked, &[]);
        never.servers = vec!["1.2.3.4:27015".into()];
        store.block(never);
        store.allow(entry("other", "1.0.0", State::Allowed, &[]));
        let items = [item(&v1, "web", "0.1.0", &[]), item(&v2, "web", "0.2.0", &[])];
        let rows = the_rows(&items, &Store::new(), &[]);
        // Delete 0.1.0 (no approval of that version): only its directory goes.
        let a = apply(Op::Delete, &rows[0], &mut store, &cl).unwrap();
        assert!(!a.store_changed && !v1.exists() && v2.exists() && a.message.contains("cl_plugins/web/0.1.0"), "{}", a.message);
        // Delete 0.2.0: its approval goes with it, the "Never" and other ids stay, and so does
        // nothing of web/.
        let a = apply(Op::Delete, &rows[1], &mut store, &cl).unwrap();
        assert!(a.store_changed && !cl.join("web").exists());
        assert_eq!(store.entries_for("web").map(|e| e.state).collect::<Vec<_>>(), [State::Blocked]);
        assert_eq!(store.entries_for("other").count(), 1);
        assert!(apply(Op::Delete, &rows[1], &mut store, &cl).is_err(), "already gone");
    }

    #[test]
    fn delete_refuses_what_isnt_a_plugin_directory() {
        let dir = tempfile::tempdir().unwrap();
        let cl = dir.path().join("cl_plugins");
        let real = installed(&cl, "web", "0.2.0");
        let mut store = Store::new();
        let mut row = the_rows(&[item(&real, "web", "0.2.0", &[])], &store, &["web"]).remove(0);
        assert!(apply(Op::Delete, &row, &mut store, &cl).unwrap_err().contains("is loaded"));
        row.loaded = false;
        for bad in [
            cl.join("web"),
            cl.join("web/.0.2.0.partial"),
            cl.join("staging/x"),
            dir.path().join("elsewhere/web/0.2.0"),
            cl.join("other/0.2.0"),
        ] {
            std::fs::create_dir_all(&bad).unwrap();
            row.dir = Some(bad.clone());
            assert!(apply(Op::Delete, &row, &mut store, &cl).is_err(), "{}", bad.display());
            assert!(bad.exists());
        }
        #[cfg(unix)]
        {
            let target = dir.path().join("precious");
            std::fs::create_dir_all(&target).unwrap();
            std::fs::write(target.join("keep"), b"k").unwrap();
            std::os::unix::fs::symlink(&target, cl.join("web/0.3.0")).unwrap();
            row.dir = Some(cl.join("web/0.3.0"));
            assert!(apply(Op::Delete, &row, &mut store, &cl).is_err(), "a link isn't followed");
            assert!(target.join("keep").exists());
        }
        row.dir = Some(real.clone());
        apply(Op::Delete, &row, &mut store, &cl).unwrap();
        assert!(!real.exists());
    }

    #[test]
    fn ops_parse_and_confirm() {
        assert_eq!(Op::parse(" delete "), Some(Op::Delete));
        assert_eq!(Op::parse("approve"), None, "nothing approves");
        let row = Row {
            id: "web".into(),
            name: "Web".into(),
            version: Some("0.2.0".into()),
            entries: vec![],
            dir: Some(PathBuf::from("/g/cl_plugins/web/0.2.0")),
            manual: false,
            installed: true,
            loaded: false,
            status: String::new(),
            files: vec![],
        };
        let l = Op::Delete.confirm_lines(&row);
        assert!(l[0] == "Delete Web 0.2.0 (web)?" && l[1].contains("cl_plugins/web/0.2.0"), "{l:?}");
    }
}
