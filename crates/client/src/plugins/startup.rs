//! Verification at every start and the start-up prompt's decisions (client-plugins.md §5.2,
//! §5.3, §7.1, §13.5): engine-free, so the unit tests run it on temporary directories with a
//! scripted [`Prompter`].
//!
//! [`run`] verifies every candidate with `plugin-pkg`, prompts for the new and changed ones (in
//! id order), records the answers in the store and returns a [`Decision`] per library. Nothing
//! here maps a library: the loader asks the gate, which reads the decisions (and re-verifies
//! anything a prompt stood in front of).

use super::prompt::{Answer, Prompt, Prompter};
use plugin_pkg::{Candidate, Change, Entry, LoadStatus, Platform, Scope, Store, Verdict, Verification, Version};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the plugins are: `<gamedir>/cl_plugins` and `<gamedir>/cl_dlls` (§4).
#[derive(Clone, Debug)]
pub struct Dirs {
    pub cl_plugins: PathBuf,
    pub cl_dlls: PathBuf,
}

impl Dirs {
    pub fn of(game_dir: &Path) -> Dirs {
        Dirs { cl_plugins: game_dir.join("cl_plugins"), cl_dlls: game_dir.join("cl_dlls") }
    }
    pub fn store(&self) -> PathBuf {
        plugin_pkg::store_path(&self.cl_plugins)
    }
}

/// What start-up checks against.
#[derive(Clone, Debug)]
pub struct Options {
    /// `cl_plugins` (§8): false loads nothing and verifies nothing.
    pub enabled: bool,
    /// `cl_plugins_verify_log`: a line per hashed file.
    pub verify_log: bool,
    pub platform: Platform,
    /// The running port's version, for `[requires] port` (§13.5).
    pub port: Version,
    /// The client extension API's major version: a package built for another can't load (§9).
    pub api: u32,
    /// Hashing threads (0: one per core).
    pub threads: usize,
    /// Hand-installed libraries the development override approves (debug builds only,
    /// client-ext-api.md §3): neither verified against the store nor prompted for.
    pub dev_names: Vec<String>,
}

/// What start-up decided for one library.
#[derive(Clone, Debug)]
pub enum Decision {
    /// Allowed, and its approval applies now (or, for `scope = "server"`, on a listed server).
    /// `fresh`: verified just now with no prompt in between, so the loader may use this
    /// verification as it is; otherwise the gate verifies again before mapping.
    Allowed { v: Box<Verification>, fresh: bool },
    /// Doesn't load; the reason, for the loader's console line.
    Skip(String),
}

/// One candidate: what `plugin-pkg` found and what start-up decided.
#[derive(Clone, Debug)]
pub struct Item {
    pub candidate: Candidate,
    /// `<name>` of `tsmod_<name>`: the package id, or a hand-installed library's name.
    pub name: String,
    /// The library to map, when there is one for this platform.
    pub library: Option<PathBuf>,
    pub decision: Decision,
    /// What verification found, for the plugins panel (`super::menu`).
    pub found: Found,
}

/// What verifying a candidate found on disk: its name, version and every file's hash (the
/// plugins panel shows them, §8).
#[derive(Clone, Debug, Default)]
pub struct Found {
    /// The manifest's name, or a hand-installed library's file name.
    pub name: String,
    pub version: Option<Version>,
    /// Every file and its SHA-256, relative to the package directory (or `cl_dlls/`).
    pub files: BTreeMap<String, plugin_pkg::Sha256>,
}

impl Found {
    pub fn of(v: &Verification) -> Found {
        let file = || v.library.as_ref().and_then(|l| l.file_name()).map_or_else(|| v.id.clone(), |f| f.to_string_lossy().into_owned());
        Found { name: v.manifest.as_ref().map_or_else(file, |m| m.name.clone()), version: v.version.clone(), files: v.files.clone() }
    }
}

/// The result of [`run`].
pub struct Outcome {
    pub items: Vec<Item>,
    /// For the console, in order.
    pub lines: Vec<String>,
    pub store: Store,
    /// False when the store couldn't be read: answers apply to this start only and aren't saved.
    pub store_writable: bool,
}

/// `<name>` of a candidate: the package directory's id, or the library's `<name>`.
pub fn candidate_name(c: &Candidate) -> String {
    match c {
        Candidate::Package { .. } => c.id(),
        Candidate::Manual { name, .. } => name.clone(),
    }
}

/// Why a verified plugin can't load in this client whatever the player says: its `[requires]
/// port` isn't met (§13.5), it was built for another extension API (§9), or its library isn't
/// one this client can map (`header`). Such plugins are skipped without a prompt.
pub fn unmet(v: &Verification, opts: &Options, header: &dyn Fn(&Path) -> Result<(), String>) -> Option<String> {
    if let Some(m) = &v.manifest {
        if !m.port_requirement_met(&opts.port) {
            let req = m.requires.port.as_ref().map(ToString::to_string).unwrap_or_default();
            return Some(format!("needs port {req}, this client is {}", opts.port));
        }
        if m.client_ext_api != opts.api {
            return Some(format!("built for client extension API version {}, this client has version {}", m.client_ext_api, opts.api));
        }
    }
    let lib = v.library.as_deref()?;
    header(lib).err()
}

/// The changes, joined for one console line.
pub fn change_summary(changes: &[Change]) -> String {
    plugin_pkg::change_lines(changes, 10).join("; ")
}

/// The source to record for an approval made at start: the one an earlier approval of this id
/// had (a server, the launcher), else "manual".
fn source_for(store: &Store, id: &str) -> String {
    store.entries_for(id).next().map_or_else(|| "manual".to_string(), |e| e.source.clone())
}

/// The scope an approval made at start keeps: an earlier allowed approval's ("Allow on this
/// server only" stays so for a rebuilt plugin), else everywhere.
fn scope_for(store: &Store, id: &str, platform: Platform) -> (Scope, Vec<String>) {
    store
        .entries_for(id)
        .find(|e| e.state == plugin_pkg::State::Allowed && e.platform == platform)
        .map_or((Scope::Always, Vec::new()), |e| (e.scope, e.servers.clone()))
}

/// The start-up prompt for `v` (§7.1).
pub fn prompt_for(v: &Verification, dirs: &Dirs, store: &Store, show_later_all: bool) -> Prompt {
    let can_allow = v.verdict != Verdict::Refuse;
    // A package's name and version; a hand-installed library's file name (it has no version).
    let file = || v.library.as_ref().and_then(|l| l.file_name()).map_or_else(|| v.id.clone(), |f| f.to_string_lossy().into_owned());
    let name = v.manifest.as_ref().map_or_else(file, |m| m.name.clone());
    let version = v.manifest.as_ref().map_or_else(String::new, |m| format!(" {}", m.version));
    let mut text = format!("{name}{version} ({})\n", v.id);
    text += &format!("Publisher: {}\n", v.trust);
    let place = if v.base.starts_with(&dirs.cl_plugins) { "installed package" } else { "hand-installed in cl_dlls" };
    match store.entries_for(&v.id).next() {
        Some(e) => text += &format!("Source: {}, {place}\n", e.source),
        None => text += &format!("Source: {place}\n"),
    }
    text += "\nWhat changed:\n";
    for l in plugin_pkg::change_lines(&v.changes, 10) {
        text += &format!("  - {l}\n");
    }
    text += "\nThis is a program that runs inside the game with your account's permissions.\n";
    if !can_allow {
        text += "It can't be approved as it is installed. Reinstall it to use it.\n";
    }
    Prompt { id: v.id.clone(), title: format!("Client plugin: {name}"), text, can_allow, show_later_all }
}

/// Verifies every candidate, prompts for the new and changed ones and decides what may load
/// (§5.2, §7.1). `store` is what `Store::load` read (`None`: it couldn't be read). `header`
/// checks that a library is one this client can map.
pub fn run(
    dirs: &Dirs,
    opts: &Options,
    loaded: std::io::Result<(Store, LoadStatus)>,
    prompter: &mut dyn Prompter,
    header: &dyn Fn(&Path) -> Result<(), String>,
) -> Outcome {
    let mut lines = Vec::new();
    let (mut store, store_writable) = match loaded {
        Ok((s, LoadStatus::SetAside { bad, error })) => {
            lines.push(format!(
                "the approval store didn't parse ({error}); set aside as {}, so every plugin asks again",
                bad.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ));
            (s, true)
        }
        Ok((s, _)) => (s, true),
        Err(e) => {
            lines.push(format!("can't read {} ({e}); answers apply to this start only", dirs.store().display()));
            (Store::new(), false)
        }
    };
    if !opts.enabled {
        lines.push("cl_plugins is 0: no client plugins load".into());
        return Outcome { items: Vec::new(), lines, store, store_writable };
    }
    let found = match plugin_pkg::discover(&dirs.cl_plugins, &dirs.cl_dlls, opts.platform.library_suffix()) {
        Ok(f) => f,
        Err(e) => {
            lines.push(format!("can't list the plugins: {e}"));
            Vec::new()
        }
    };
    let found: Vec<Candidate> =
        found.into_iter().filter(|c| !matches!(c, Candidate::Manual { name, .. } if opts.dev_names.contains(name))).collect();
    let roots = plugin_pkg::builtin_roots();
    let mut verified: Vec<(Candidate, Verification)> =
        found.into_iter().map(|c| (c.clone(), plugin_pkg::verify(&c, opts.platform, &store, &roots, opts.threads))).collect();
    if !verified.is_empty() {
        let files: usize = verified.iter().map(|(_, v)| v.files.len()).sum();
        let ms: u128 = verified.iter().map(|(_, v)| v.elapsed.as_millis()).sum();
        lines.push(format!("verified {} plugin(s), {files} file(s), in {ms} ms", verified.len()));
    }
    if opts.verify_log {
        for (_, v) in &verified {
            for (path, hash) in &v.files {
                lines.push(format!("  {hash} {}/{path}", v.id));
            }
        }
    }
    // §7.1: one box each, in id order.
    verified.sort_by(|a, b| a.1.id.cmp(&b.1.id));

    let mut decisions: Vec<Option<Decision>> = Vec::with_capacity(verified.len());
    for (_, v) in &verified {
        decisions.push(match v.verdict {
            Verdict::Blocked => Some(Decision::Skip("blocked (\"Never\"); cl_plugins_menu can unblock it".into())),
            Verdict::Load | Verdict::Prompt => unmet(v, opts, header).map(Decision::Skip),
            Verdict::Refuse => None,
        });
    }
    // A hand-installed library that isn't one this client can map never gets a box: the loader
    // reports it from its header (plugin_lib) and doesn't ask.
    let to_ask = verified.iter().zip(&decisions).filter(|((_, v), d)| d.is_none() && v.verdict != Verdict::Load).count();
    let mut later_all = false;
    let mut store_changed = false;
    for ((_, v), d) in verified.iter().zip(decisions.iter_mut()) {
        if d.is_some() {
            continue;
        }
        if v.verdict == Verdict::Load {
            *d = Some(Decision::Allowed { v: Box::new(v.clone()), fresh: true });
            continue;
        }
        let answer = if later_all { Answer::Later } else { prompter.ask(&prompt_for(v, dirs, &store, to_ask > 3)) };
        let source = source_for(&store, &v.id);
        let (scope, servers) = scope_for(&store, &v.id, v.platform);
        *d = Some(match answer {
            Answer::Allow => match v.approval(&source, scope, servers) {
                Some(entry) => {
                    store.allow(entry.clone());
                    store_changed = true;
                    let mut v = v.clone();
                    v.entry = Some(entry);
                    v.verdict = Verdict::Load;
                    Decision::Allowed { v: Box::new(v), fresh: false }
                }
                None => Decision::Skip(format!("can't be approved as installed: {}", change_summary(&v.changes))),
            },
            Answer::Never => {
                store.block(v.blocking(&source, Vec::new()));
                store_changed = true;
                Decision::Skip("blocked (\"Never\"); cl_plugins_menu can unblock it".into())
            }
            Answer::Later | Answer::LaterAll => {
                later_all |= answer == Answer::LaterAll;
                if v.verdict == Verdict::Refuse {
                    Decision::Skip(format!("can't be approved as installed: {}", change_summary(&v.changes)))
                } else {
                    Decision::Skip(format!("not approved ({}); asks again at the next start", change_summary(&v.changes)))
                }
            }
        });
    }
    if store_changed
        && store_writable
        && let Err(e) = store.save(&dirs.store())
    {
        lines.push(format!("can't save {} ({e}); this start's answers aren't kept", dirs.store().display()));
    }
    let items = verified
        .into_iter()
        .zip(decisions)
        .map(|((candidate, v), d)| Item {
            name: candidate_name(&candidate),
            library: v.library.clone(),
            found: Found::of(&v),
            candidate,
            decision: d.unwrap_or_else(|| Decision::Skip("not decided".into())),
        })
        .collect();
    Outcome { items, lines, store, store_writable }
}

/// Re-verifies one candidate before it's mapped (the player answered a prompt in between, or a
/// server-scoped plugin is loading on connect): the allowed entry that applies on `server`, or
/// why it can't load.
pub fn recheck(
    candidate: &Candidate,
    opts: &Options,
    store: &Store,
    server: Option<&str>,
    header: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Verification, String> {
    let v = plugin_pkg::verify(candidate, opts.platform, store, &plugin_pkg::builtin_roots(), opts.threads);
    usable(v, opts, server, header)
}

/// Whether a verification lets the plugin load on `server`.
pub fn usable(
    v: Verification,
    opts: &Options,
    server: Option<&str>,
    header: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<Verification, String> {
    match v.verdict {
        Verdict::Load => {}
        Verdict::Blocked => return Err("blocked (\"Never\")".into()),
        Verdict::Prompt => return Err(format!("not approved ({})", change_summary(&v.changes))),
        Verdict::Refuse => return Err(format!("can't be approved as installed: {}", change_summary(&v.changes))),
    }
    if let Some(why) = unmet(&v, opts, header) {
        return Err(why);
    }
    match &v.entry {
        Some(e) if e.applies_to(server) => Ok(v),
        Some(e) => Err(server_only(e)),
        None => Err("no approval".into()),
    }
}

/// The reason a `scope = "server"` plugin waits.
pub fn server_only(e: &Entry) -> String {
    format!("approved for {} only; loads on connecting there", e.servers.join(", "))
}

/// The value an archived cvar has in `config.cfg` (`name "value"`, the last one wins). The engine
/// executes `config.cfg` only after `Initialize`, so start-up reads `cl_plugins` from it (§8).
pub fn archived_cvar(config: &str, name: &str) -> Option<String> {
    config.lines().rev().find_map(|l| {
        let l = l.trim();
        let (key, rest) = l.split_once(char::is_whitespace)?;
        key.eq_ignore_ascii_case(name).then(|| rest.trim().trim_matches('"').to_string())
    })
}

/// A cvar value as a switch: anything but a number other than 0 is off, like the engine's
/// `value`.
pub fn switch(value: &str) -> bool {
    value.trim().parse::<f32>().is_ok_and(|v| v != 0.0)
}

/// Every decision keyed by library path, for the gate.
pub fn by_library(items: &[Item]) -> BTreeMap<PathBuf, usize> {
    items.iter().enumerate().filter_map(|(i, it)| it.library.clone().map(|l| (l, i))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::prompt::Prompt;
    use plugin_pkg::{FileEntry, Sha256, Trust};
    use std::fs;

    const P: Platform = Platform::LinuxX86_64;

    /// Answers by id, recording every prompt.
    struct Script {
        answers: Vec<(&'static str, Answer)>,
        asked: Vec<Prompt>,
    }

    impl Prompter for Script {
        fn ask(&mut self, p: &Prompt) -> Answer {
            self.asked.push(p.clone());
            self.answers.iter().find(|(id, _)| *id == p.id).map_or(Answer::Later, |(_, a)| *a)
        }
    }

    fn script(answers: &[(&'static str, Answer)]) -> Script {
        Script { answers: answers.to_vec(), asked: Vec::new() }
    }

    fn opts() -> Options {
        Options { enabled: true, verify_log: false, platform: P, port: Version::new(3, 0, 8), api: 1, threads: 1, dev_names: Vec::new() }
    }

    fn ok(_: &Path) -> Result<(), String> {
        Ok(())
    }

    struct Env {
        root: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Env {
            let e = Env { root: tempfile::tempdir().unwrap() };
            fs::create_dir_all(e.dirs().cl_dlls).unwrap();
            e
        }
        fn dirs(&self) -> Dirs {
            Dirs::of(self.root.path())
        }
        fn manual(&self, name: &str, bytes: &[u8]) {
            fs::write(self.dirs().cl_dlls.join(format!("tsmod_{name}_amd64.so")), bytes).unwrap();
        }
        /// Installs an unsigned package `id` 1.0.0 with a library and a data file.
        fn package(&self, id: &str, extra_manifest: &str) -> PathBuf {
            let src = self.root.path().join(format!("src-{id}"));
            let lib = format!("lib/tsmod_{id}.so");
            fs::create_dir_all(src.join("lib")).unwrap();
            fs::write(src.join(&lib), b"library").unwrap();
            fs::write(src.join("data.txt"), b"data").unwrap();
            let files: Vec<FileEntry> =
                [lib.as_str(), "data.txt"].iter().map(|p| plugin_pkg::build::file_entry(&src, p, None).unwrap()).collect();
            let mut m = format!(
                "format = 1\nid = \"{id}\"\nname = \"Plugin {id}\"\nversion = \"1.0.0\"\nclient_ext_api = 1\n{extra_manifest}\n[platforms]\nlinux-x86_64 = \"{lib}\"\n\n"
            );
            for f in &files {
                m += &format!("[[files]]\npath = \"{}\"\nsize = {}\nsha256 = \"{}\"\n", f.path, f.size, f.sha256);
            }
            let pkg = self.root.path().join(format!("{id}.tspkg"));
            let out = plugin_pkg::build::write_package(fs::File::create(&pkg).unwrap(), m.as_bytes(), None, None, &src).unwrap();
            drop(out);
            let dest = self.dirs().cl_plugins.join(id).join("1.0.0");
            plugin_pkg::Package::open(&pkg).unwrap().extract(&dest, P).unwrap();
            dest
        }
        fn run(&self, p: &mut Script) -> Outcome {
            self.run_with(p, &opts())
        }
        fn run_with(&self, p: &mut Script, o: &Options) -> Outcome {
            run(&self.dirs(), o, Store::load(&self.dirs().store()), p, &ok)
        }
    }

    fn decision<'a>(o: &'a Outcome, name: &str) -> &'a Decision {
        &o.items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name}")).decision
    }

    fn allowed(o: &Outcome, name: &str) -> bool {
        matches!(decision(o, name), Decision::Allowed { .. })
    }

    fn skip(o: &Outcome, name: &str) -> String {
        match decision(o, name) {
            Decision::Skip(why) => why.clone(),
            d => panic!("{name}: {d:?}"),
        }
    }

    #[test]
    fn new_plugins_prompt_and_allow_is_remembered() {
        let env = Env::new();
        env.manual("dev", b"build 1");
        env.package("web", "");
        let mut p = script(&[("manual:dev", Answer::Allow), ("web", Answer::Later)]);
        let o = env.run(&mut p);
        assert_eq!(p.asked.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["manual:dev", "web"], "id order");
        assert!(p.asked.iter().all(|a| a.can_allow && !a.show_later_all));
        assert!(p.asked[0].text.contains("new plugin") && p.asked[0].text.contains("account's permissions"));
        assert!(p.asked[0].text.contains("Unsigned"), "{}", p.asked[0].text);
        assert!(p.asked[0].text.starts_with("tsmod_dev_amd64.so (manual:dev)\n"), "{}", p.asked[0].text);
        assert_eq!(p.asked[0].title, "Client plugin: tsmod_dev_amd64.so");
        assert!(matches!(decision(&o, "dev"), Decision::Allowed { fresh: false, .. }), "re-verified before mapping");
        assert!(skip(&o, "web").contains("not approved (new plugin)"));
        // The store was saved: next start loads dev without asking, and asks about web again.
        let mut p = script(&[]);
        let o = env.run(&mut p);
        assert!(matches!(decision(&o, "dev"), Decision::Allowed { fresh: true, .. }));
        assert_eq!(p.asked.len(), 1);
        // A rebuild asks again and names the change.
        env.manual("dev", b"build 2");
        let mut p = script(&[]);
        let o = env.run(&mut p);
        assert!(skip(&o, "dev").contains("changed: tsmod_dev_amd64.so"));
        assert!(p.asked.iter().any(|a| a.id == "manual:dev" && a.text.contains("changed: tsmod_dev_amd64.so")));
    }

    #[test]
    fn never_blocks_and_stops_asking() {
        let env = Env::new();
        env.manual("nope", b"x");
        let o = env.run(&mut script(&[("manual:nope", Answer::Never)]));
        assert!(skip(&o, "nope").contains("blocked"));
        let mut p = script(&[]);
        let o = env.run(&mut p);
        assert!(p.asked.is_empty(), "a blocked plugin isn't asked about");
        assert!(skip(&o, "nope").contains("blocked"));
    }

    #[test]
    fn tampered_packages_get_no_allow() {
        let env = Env::new();
        let dirs: Vec<PathBuf> = ["a", "b", "c"].iter().map(|id| env.package(id, "")).collect();
        env.run(&mut script(&[("a", Answer::Allow), ("b", Answer::Allow), ("c", Answer::Allow)]));
        assert!(env.run(&mut script(&[])).items.iter().all(|i| matches!(i.decision, Decision::Allowed { fresh: true, .. })));
        fs::write(dirs[0].join("files/data.txt"), b"dat4").unwrap();
        fs::write(dirs[1].join("files/extra.txt"), b"x").unwrap();
        fs::remove_file(dirs[2].join("files/data.txt")).unwrap();
        // Even "Allow" can't approve them.
        let mut p = script(&[("a", Answer::Allow), ("b", Answer::Allow), ("c", Answer::Allow)]);
        let o = env.run(&mut p);
        assert_eq!(p.asked.len(), 3);
        assert!(p.asked.iter().all(|a| !a.can_allow && a.text.contains("Reinstall")));
        assert!(skip(&o, "a").contains("changed: files/data.txt"));
        assert!(skip(&o, "b").contains("extra file: files/extra.txt"));
        assert!(skip(&o, "c").contains("missing: files/data.txt"));
        let (store, _) = Store::load(&env.dirs().store()).unwrap();
        assert_eq!(store.plugins.len(), 3, "nothing approved the tampered files");
    }

    #[test]
    fn requirements_skip_without_a_prompt() {
        let env = Env::new();
        env.package("old", "\n[requires]\nport = \"<3.0\"\n");
        env.package("fits", "\n[requires]\nport = \">=3.0.8, <3.1\"\n");
        let mut api2 = opts();
        api2.api = 2;
        let mut p = script(&[("fits", Answer::Allow)]);
        let o = env.run(&mut p);
        assert_eq!(p.asked.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["fits"]);
        assert_eq!(skip(&o, "old"), "needs port <3.0, this client is 3.0.8");
        assert!(allowed(&o, "fits"));
        let o = env.run_with(&mut script(&[]), &api2);
        assert!(skip(&o, "fits").contains("API version 1, this client has version 2"));
        // A library this client can't map isn't prompted for.
        env.manual("junk", b"junk");
        let mut p = script(&[]);
        let o = run(&env.dirs(), &opts(), Store::load(&env.dirs().store()), &mut p, &|l: &Path| {
            if l.to_string_lossy().contains("junk") { Err("not a library".into()) } else { Ok(()) }
        });
        assert!(p.asked.iter().all(|a| a.id != "manual:junk"));
        assert!(skip(&o, "junk").contains("not a library"));
    }

    #[test]
    fn later_for_all_and_disabled() {
        let env = Env::new();
        for n in ["a", "b", "c", "d"] {
            env.manual(n, n.as_bytes());
        }
        let mut p = script(&[("manual:a", Answer::LaterAll)]);
        let o = env.run(&mut p);
        assert_eq!(p.asked.len(), 1, "the rest aren't asked");
        assert!(p.asked[0].show_later_all, "more than three to ask about");
        assert_eq!(o.items.iter().filter(|i| matches!(i.decision, Decision::Skip(_))).count(), 4);

        let mut off = opts();
        off.enabled = false;
        let mut p = script(&[]);
        let o = env.run_with(&mut p, &off);
        assert!(o.items.is_empty() && p.asked.is_empty());
        assert!(o.lines.iter().any(|l| l.contains("cl_plugins is 0")));
    }

    #[test]
    fn dev_override_names_are_left_alone() {
        let env = Env::new();
        env.manual("test", b"x");
        let mut o = opts();
        o.dev_names = vec!["test".into()];
        let mut p = script(&[]);
        let out = env.run_with(&mut p, &o);
        assert!(p.asked.is_empty() && out.items.is_empty());
    }

    #[test]
    fn server_scoped_approvals_wait() {
        let env = Env::new();
        env.manual("srv", b"x");
        env.run(&mut script(&[("manual:srv", Answer::Allow)]));
        let path = env.dirs().store();
        let (mut store, _) = Store::load(&path).unwrap();
        store.plugins[0].scope = Scope::Server;
        store.plugins[0].servers = vec!["203.0.113.5:27015".into()];
        store.save(&path).unwrap();
        let o = env.run(&mut script(&[]));
        let it = o.items.iter().find(|i| i.name == "srv").unwrap();
        let Decision::Allowed { v, .. } = &it.decision else { panic!() };
        assert!(usable((**v).clone(), &opts(), None, &ok).unwrap_err().contains("203.0.113.5:27015 only"));
        assert!(recheck(&it.candidate, &opts(), &o.store, Some("203.0.113.5:27015"), &ok).is_ok());
        assert!(recheck(&it.candidate, &opts(), &o.store, Some("10.0.0.1:27015"), &ok).is_err());
        // A rebuild approved at start stays "this server only".
        env.manual("srv", b"y");
        env.run(&mut script(&[("manual:srv", Answer::Allow)]));
        let (store, _) = Store::load(&path).unwrap();
        let e = store.entries_for("manual:srv").next().unwrap();
        assert_eq!((e.scope, e.servers.as_slice()), (Scope::Server, &["203.0.113.5:27015".to_string()][..]));
    }

    #[test]
    fn a_bad_store_is_set_aside_and_everything_asks() {
        let env = Env::new();
        env.manual("dev", b"x");
        fs::create_dir_all(&env.dirs().cl_plugins).unwrap();
        fs::write(env.dirs().store(), "format = 7\n").unwrap();
        let mut p = script(&[]);
        let o = env.run(&mut p);
        assert!(o.lines.iter().any(|l| l.contains("set aside as approved.toml.bad")));
        assert_eq!(p.asked.len(), 1);
    }

    #[test]
    fn the_verify_log_names_every_file() {
        let env = Env::new();
        env.manual("dev", b"x");
        let mut o = opts();
        o.verify_log = true;
        let out = env.run_with(&mut script(&[]), &o);
        let h = Sha256::of_bytes(b"x");
        assert!(out.lines.iter().any(|l| l.contains(&format!("{h} manual:dev/tsmod_dev_amd64.so"))), "{:?}", out.lines);
        assert!(out.lines.iter().any(|l| l.starts_with("verified 1 plugin(s), 1 file(s)")));
    }

    #[test]
    fn prompts_show_trust_and_source() {
        let env = Env::new();
        env.package("web", "");
        let mut p = script(&[]);
        env.run(&mut p);
        let t = &p.asked[0];
        assert_eq!(t.title, "Client plugin: Plugin web");
        assert!(t.text.starts_with("Plugin web 1.0.0 (web)\n"), "{}", t.text);
        assert!(t.text.contains("Source: installed package"));
        assert!(!Trust::Unsigned.is_signed());
    }

    #[test]
    fn config_cfg_values() {
        let cfg = "// comment\ncl_plugins \"1\"\nbind \"a\" \"+left\"\nCL_PLUGINS \"0\"\ncl_plugins_verify_log 1\n";
        assert_eq!(archived_cvar(cfg, "cl_plugins").as_deref(), Some("0"));
        assert_eq!(archived_cvar(cfg, "cl_plugins_verify_log").as_deref(), Some("1"));
        assert_eq!(archived_cvar(cfg, "cl_plugins_download"), None);
        assert!(switch("1") && switch("2.5") && !switch("0") && !switch("") && !switch("yes"));
    }
}
