//! The client's side of `cargo xtask tspkg` (issue #299, client-plugins.md §11 e2e 2–4, §13.1):
//! the e2e build of the port's FWGS client (`fwgs` root; cards answered by `TSRS_E2E_CARDS`,
//! `crates/client/src/plugins/offers/e2e.rs`) against each Linux root's dedicated server, with the
//! packages served over plain HTTP by a FastDL stand-in started here ([`FastDl`], on
//! `sv_downloadurl`'s `127.0.0.1:27999/ts`), which can serve other bytes than the server checked.
//! The client's approval store is set aside for the run and put back afterwards.
//!
//! 4. **Offers.** Four offers of quiet stub extensions (crates/tsmod-client-test):
//!    `quietoffer` 0.1.0 (signed; the card is answered "Allow on this server only"), `quiettamper`
//!    (FastDL serves a package with one byte changed: `failed`, after its card), `quietbadsig`
//!    (FastDL serves a broken `.manifest.sig`: `failed`, no card, the package never fetched) and
//!    `quietunsigned` (unsigned over plain HTTP: `failed`, no card). Then `quietoffer` must answer
//!    `offered`, `downloading`, `loaded`, be installed with its approval (scope `server`, every
//!    file's hash) and load; nothing of the others may be left in `cl_plugins/`.
//! 5. **Update.** The same server offers `quietoffer` 0.2.0: 0.1.0 loads on connecting (approved
//!    for this server only), the update card shows "0.1.0 -> 0.2.0" and is answered "Install
//!    update"; the client answers `restart` and its "Restart now" card restarts through
//!    `relaunch::restart`, which starts `ts-launcher-fake relaunch ... --reason plugin --plugin
//!    quietoffer@0.2.0` from `ts/launcher.toml`.
//! 6. **Op 5 and `have`.** A server with `sv_client_version ">=99.0.0"` offering 0.2.0: at start
//!    the superseded 0.1.0 is removed and 0.2.0 loads; the offer answers `have`, the `tspkg list`
//!    names it, and the port card's "Restart to update" sends `tspkg tsrs ... restart` and starts
//!    the fake launcher with `--reason port --port-version >=99.0.0`.

use crate::Ctx;
use crate::roots::Root;
use plugin_pkg::{PublicKey, Sha256, SigningKey, Store};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The FastDL stand-in's state: the game directory it serves under `/ts/`, paths whose bytes it
/// replaces, and every path asked for.
#[derive(Default)]
pub struct FastDlState {
    pub root: PathBuf,
    pub overrides: BTreeMap<String, Vec<u8>>,
    pub requests: Vec<String>,
}

/// A static HTTP/1.1 server on 127.0.0.1:27999 for the run (one thread per connection).
#[derive(Clone)]
pub struct FastDl(pub Arc<Mutex<FastDlState>>);

impl FastDl {
    pub fn start() -> Result<FastDl, String> {
        let l = TcpListener::bind("127.0.0.1:27999").map_err(|e| format!("FastDL on 127.0.0.1:27999: {e}"))?;
        let state = Arc::new(Mutex::new(FastDlState::default()));
        let s = state.clone();
        std::thread::spawn(move || {
            for conn in l.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || serve_one(conn, &s));
            }
        });
        Ok(FastDl(state))
    }

    fn set(&self, root: &Path, overrides: BTreeMap<String, Vec<u8>>) {
        let mut s = self.0.lock().unwrap();
        s.root = root.to_path_buf();
        s.overrides = overrides;
        s.requests.clear();
    }

    fn requests(&self) -> Vec<String> {
        self.0.lock().unwrap().requests.clone()
    }
}

fn serve_one(conn: std::net::TcpStream, state: &Mutex<FastDlState>) {
    let mut r = std::io::BufReader::new(match conn.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
    }
    let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
    let body = {
        let mut s = state.lock().unwrap();
        s.requests.push(path.clone());
        match s.overrides.get(&path) {
            Some(b) => Some(b.clone()),
            None => path
                .strip_prefix("/ts/")
                .filter(|rel| !rel.split('/').any(|p| p == ".." || p.is_empty()))
                .and_then(|rel| fs::read(s.root.join(rel)).ok()),
        }
    };
    let mut conn = conn;
    let _ = match body {
        Some(b) => {
            write!(conn, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).and_then(|()| conn.write_all(&b))
        }
        None => write!(conn, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    };
}

/// The quiet stub packages' ids.
const OFFER: &str = "quietoffer";
const TAMPER: &str = "quiettamper";
const BADSIG: &str = "quietbadsig";
const UNSIGNED: &str = "quietunsigned";
const IDS: [&str; 4] = [OFFER, TAMPER, BADSIG, UNSIGNED];

/// Builds `<gamedir>/plugins/<id>-<version>.tspkg` around the stub extension `lib`, writes its
/// siblings and returns its `[[offer]]` entry.
/// The client's platform, library suffix and the stub extension the packages carry.
struct Stub<'a> {
    plat: &'a str,
    sfx: &'a str,
    lib: &'a Path,
}

fn package(ctx: &Ctx, gamedir: &Path, stub: &Stub, id: &str, version: &str, signed: bool) -> Result<String, String> {
    let Stub { plat, sfx, lib } = *stub;
    let src = ctx.repo.join("target").join("tspkg-client").join(format!("{id}-{version}"));
    let _ = fs::remove_dir_all(&src);
    let lib_rel = format!("lib/{plat}/tsmod_{id}{sfx}");
    fs::create_dir_all(src.join(format!("lib/{plat}"))).map_err(|e| e.to_string())?;
    fs::create_dir_all(src.join("data")).map_err(|e| e.to_string())?;
    fs::copy(lib, src.join(&lib_rel)).map_err(|e| format!("{}: {e}", lib.display()))?;
    fs::write(src.join("data/readme.txt"), format!("{id} {version}\n")).map_err(|e| e.to_string())?;
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut m = format!(
        "format = 1\nid = \"{id}\"\nname = \"xtask {id}\"\nversion = \"{version}\"\ndescription = \"cargo xtask tspkg's client-side stub\"\nhomepage = \"https://example.org/{id}\"\nclient_ext_api = {}\n",
        ts_client_ext_api::VERSION
    );
    if signed {
        m += &format!("\n[publisher]\nname = \"xtask\"\nkey = \"{}\"\n", PublicKey::of(&key));
    }
    m += &format!("\n[platforms]\n{plat} = \"{lib_rel}\"\n\n[declares]\ndata = true\n");
    for p in [lib_rel.as_str(), "data/readme.txt"] {
        let f = plugin_pkg::build::file_entry(&src, p, None).map_err(|e| format!("{p}: {e}"))?;
        m += &format!("\n[[files]]\npath = \"{}\"\nsize = {}\nsha256 = \"{}\"\n", f.path, f.size, f.sha256);
    }
    let sig = signed.then(|| plugin_pkg::sign_manifest(&key, m.as_bytes()));
    let plugins = gamedir.join("plugins");
    fs::create_dir_all(&plugins).map_err(|e| e.to_string())?;
    let pkg = plugins.join(format!("{id}-{version}.tspkg"));
    for suffix in [".manifest.toml", ".manifest.sig", ".publisher.cert"] {
        let _ = fs::remove_file(plugins.join(format!("{id}-{version}{suffix}")));
    }
    let file = fs::File::create(&pkg).map_err(|e| e.to_string())?;
    plugin_pkg::build::write_package(file, m.as_bytes(), sig.as_ref().map(|s| s.as_slice()), None, &src).map_err(|e| e.to_string())?;
    crate::tspkg::plugin_offer(&pkg, false)
}

fn remove_packages(gamedir: &Path) {
    for e in fs::read_dir(gamedir.join("plugins")).into_iter().flatten().flatten() {
        if IDS.iter().any(|id| e.file_name().to_string_lossy().starts_with(&format!("{id}-"))) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// The client's store while the run uses its own.
fn store_paths(gamedir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let s = plugin_pkg::store_path(&gamedir.join("cl_plugins"));
    (s.clone(), s.with_file_name("approved.toml.xtask-tspkg-backup"), s.with_file_name("approved.toml.xtask-tspkg-none"))
}

fn remove_client_fixtures(gamedir: &Path) {
    for id in IDS {
        let _ = fs::remove_dir_all(gamedir.join("cl_plugins").join(id));
        let _ = fs::remove_dir_all(gamedir.join("cl_plugins_data").join(id));
    }
    let _ = fs::remove_dir_all(gamedir.join("cl_plugins").join("staging"));
    let _ = fs::remove_file(gamedir.join("launcher.toml"));
}

/// Sets the client's store aside (or notes there was none) and removes what a cut-short run left.
pub fn set_aside(gamedir: &Path) {
    restore(gamedir);
    let (store, backup, none) = store_paths(gamedir);
    fs::create_dir_all(gamedir.join("cl_plugins")).unwrap();
    if store.exists() {
        fs::rename(&store, backup).unwrap();
    } else {
        fs::write(none, b"").unwrap();
    }
}

/// Puts the client's own store back and removes the run's files.
pub fn restore(gamedir: &Path) {
    let (store, backup, none) = store_paths(gamedir);
    if backup.exists() {
        let _ = fs::rename(&backup, &store);
    } else if none.exists() {
        let _ = fs::remove_file(&store);
        let _ = fs::remove_file(&none);
    }
    remove_client_fixtures(gamedir);
}

/// Flips one byte in the middle.
fn tampered(mut b: Vec<u8>) -> Vec<u8> {
    let n = b.len() / 2;
    b[n] ^= 0x55;
    b
}

impl Ctx {
    /// Scenarios 4 to 6 (module docs) against `root`'s server on `port` and `port + 1`.
    pub(crate) fn tspkg_client(&self, root: &Root, fwgs: &Root, port: u16, dl: &FastDl, dev: &str) -> Vec<(String, bool)> {
        let t = fwgs.target();
        let plat = t.platform;
        let sfx = crate::client_plugins::suffix(&t);
        let lib = self.client_stub(&t);
        let stub = Stub { plat, sfx, lib: &lib };
        let gamedir = root.path.join("ts");
        let client_dir = fwgs.path.join("ts");
        let work = self.repo.join("target").join("tspkg-client");
        fs::create_dir_all(&work).unwrap();
        let cards = work.join(format!("{}-cards.txt", root.name));
        let shown_path = PathBuf::from(format!("{}.shown", cards.display()));
        let fake_log = work.join(format!("{}-launcher.log", root.name));
        let fake_exe = fwgs.path.join(crate::relaunch::LAUNCHER_DIR).join(crate::relaunch::fake_name(&t));
        let mut c: Vec<(String, bool)> = Vec::new();
        let env = vec![("TSRS_E2E_CARDS", cards.display().to_string())];
        let begin = |answers: &str| {
            fs::write(&cards, answers).unwrap();
            let _ = fs::remove_file(&shown_path);
            let _ = fs::remove_file(&fake_log);
        };
        let shown = || fs::read_to_string(&shown_path).unwrap_or_default();
        let w = format!("{}\n", "wait;".repeat(20)).repeat(10);
        let middle = format!("{w}{w}{w}cl_plugins_list\n{w}");
        let addr = format!("127.0.0.1:{port}");
        set_aside(&client_dir);
        let store = || Store::load(&plugin_pkg::store_path(&client_dir.join("cl_plugins"))).map(|(s, _)| s).unwrap_or_default();

        // 4: offers.
        let mut offers = String::new();
        let mut setup = Ok(());
        for (id, signed) in [(OFFER, true), (TAMPER, true), (BADSIG, true), (UNSIGNED, false)] {
            match package(self, &gamedir, &stub, id, "0.1.0", signed) {
                Ok(e) => offers += &format!("{e}\n"),
                Err(e) => setup = Err(format!("{id}: {e}")),
            }
        }
        c.push(("4: the packages were built".into(), setup.is_ok()));
        if let Err(e) = &setup {
            crate::say!("   {e}");
            restore(&client_dir);
            return c;
        }
        fs::write(gamedir.join("plugin_offers.toml"), &offers).unwrap();
        let plugins = gamedir.join("plugins");
        let mut over = BTreeMap::new();
        over.insert(
            format!("/ts/plugins/{TAMPER}-0.1.0.tspkg"),
            tampered(fs::read(plugins.join(format!("{TAMPER}-0.1.0.tspkg"))).unwrap()),
        );
        over.insert(
            format!("/ts/plugins/{BADSIG}-0.1.0.manifest.sig"),
            tampered(fs::read(plugins.join(format!("{BADSIG}-0.1.0.manifest.sig"))).unwrap()),
        );
        dl.set(&gamedir, over);
        begin(&format!("offer:{OFFER}=server\noffer=install\n"));
        crate::say!("== {}: the client's offers, downloads and cards (port {port})", root.name);
        if let Some(server) = self.version_server(root, port, dev) {
            let client = self.fwgs_client_script_env(root, fwgs, port, "tspkg-client-offers", &middle, &env);
            let log = self.version_server_stop(root, server);
            let s = shown();
            let req = dl.requests();
            let st = |id: &str, state: &str| log.contains(&format!(": {id} 0.1.0 {state}"));
            c.push((
                "4: the offer answers offered, downloading, loaded".into(),
                st(OFFER, "offered") && st(OFFER, "downloading") && st(OFFER, "loaded"),
            ));
            c.push((
                "4: its card was shown with the server, publisher and native-code line".into(),
                s.contains(&format!(
                    "kind=offer id={OFFER} buttons=Install and allow|Allow on this server only|Not now|Never from this server answer=server"
                )) && s.contains(&addr)
                    && s.contains("Signed by an unknown publisher")
                    && s.contains("account's permissions"),
            ));
            c.push(("4: the client sent tspkg list".into(), log.contains("plugins: none")));
            c.push((
                "4: the extension loaded late".into(),
                client.contains(&format!("loaded tsmod_{OFFER}{sfx}")) && client.contains("quiet stub init"),
            ));
            let dir = client_dir.join("cl_plugins").join(OFFER).join("0.1.0");
            c.push((
                "4: installed in cl_plugins/quietoffer/0.1.0".into(),
                dir.join("files").join(format!("lib/{plat}/tsmod_{OFFER}{sfx}")).is_file(),
            ));
            let st_ = store();
            let entry = st_.entries_for(OFFER).next().cloned();
            c.push((
                format!("4: approved for {addr} only, from the server, with every file's hash"),
                entry.as_ref().is_some_and(|e| {
                    e.scope == plugin_pkg::Scope::Server
                        && e.servers == [addr.clone()]
                        && e.source == format!("server {addr}")
                        && e.files.len() == 4
                        && e.files.iter().all(|(p, h)| Sha256::of_file(&dir.join(p)).is_ok_and(|(f, _)| f == *h))
                }),
            ));
            c.push((
                "4: a tampered download fails after its card".into(),
                st(TAMPER, "failed") && log.contains("SHA-256") && s.contains(&format!("id={TAMPER}")),
            ));
            c.push((
                "4: a broken signature fails before any card or download".into(),
                st(BADSIG, "failed")
                    && log.contains("signature")
                    && !s.contains(&format!("id={BADSIG}"))
                    && !req.iter().any(|r| r.ends_with(&format!("{BADSIG}-0.1.0.tspkg"))),
            ));
            c.push((
                "4: an unsigned package over plain HTTP fails before any card or download".into(),
                st(UNSIGNED, "failed")
                    && log.contains("unsigned")
                    && !s.contains(&format!("id={UNSIGNED}"))
                    && !req.iter().any(|r| r.ends_with(&format!("{UNSIGNED}-0.1.0.tspkg"))),
            ));
            c.push((
                "4: nothing of the refused packages is left in cl_plugins".into(),
                [TAMPER, BADSIG, UNSIGNED].iter().all(|id| !client_dir.join("cl_plugins").join(id).exists())
                    && fs::read_dir(client_dir.join("cl_plugins/staging")).map_or(true, |mut d| d.next().is_none()),
            ));
            c.push(("4: no panic".into(), !client.contains("PANIC") && !log.contains("PANIC")));
        } else {
            c.push(("4: the server answered".into(), false));
        }

        // 5: an update of the loaded plugin, then restart through the fake launcher.
        remove_packages(&gamedir);
        let entry = package(self, &gamedir, &stub, OFFER, "0.2.0", true).unwrap_or_default();
        fs::write(gamedir.join("plugin_offers.toml"), &entry).unwrap();
        dl.set(&gamedir, BTreeMap::new());
        fs::write(
            client_dir.join("launcher.toml"),
            format!(
                "format = 1\nlauncher = {:?}\nlauncher_version = \"0.0.0-fake\"\ninstance = \"e2e\"\nroot = {:?}\n",
                fake_exe.display().to_string(),
                fwgs.path.display().to_string()
            ),
        )
        .unwrap();
        fs::write(fake_exe.with_file_name("ts-launcher-fake.cfg"), format!("log={}\nanswer=ok\n", fake_log.display())).unwrap();
        begin(&format!("update:{OFFER}=install\nrestart=ok\n"));
        crate::say!("== {}: an update of a loaded plugin, restart to finish (port {port})", root.name);
        if let Some(server) = self.version_server(root, port, dev) {
            let client = self.fwgs_client_script_env(root, fwgs, port, "tspkg-client-update", &middle, &env);
            let log = self.version_server_stop(root, server);
            let s = shown();
            let fake = fs::read_to_string(&fake_log).unwrap_or_default();
            c.push(("5: 0.1.0 loaded on connecting to its server".into(), client.contains(&format!("loaded tsmod_{OFFER}{sfx}"))));
            c.push((
                "5: the update card shows 0.1.0 -> 0.2.0".into(),
                s.contains(&format!("kind=update id={OFFER}")) && s.contains("0.1.0 -> 0.2.0") && s.contains("answer=install"),
            ));
            c.push((
                "5: downloading, then restart (the old version is loaded)".into(),
                log.contains(&format!(": {OFFER} 0.2.0 downloading")) && log.contains(&format!(": {OFFER} 0.2.0 restart")),
            ));
            c.push((
                "5: the restart card was shown and answered".into(),
                s.contains(&format!("kind=restart id={OFFER}")) && s.contains("answer=restart"),
            ));
            c.push((
                format!("5: relaunch started ts-launcher-fake relaunch --reason plugin --plugin {OFFER}@0.2.0"),
                [
                    "arg relaunch",
                    "arg --connect",
                    &format!("arg {addr}"),
                    "arg --reason",
                    "arg plugin",
                    "arg --plugin",
                    &format!("arg {OFFER}@0.2.0"),
                ]
                .iter()
                .all(|w| fake.contains(w)),
            ));
            let st_ = store();
            c.push((
                "5: the store has 0.2.0 only, everywhere".into(),
                st_.entries_for(OFFER).count() == 1
                    && st_.entries_for(OFFER).all(|e| e.version.to_string() == "0.2.0" && e.scope == plugin_pkg::Scope::Always),
            ));
            let id_dir = client_dir.join("cl_plugins").join(OFFER);
            c.push((
                "5: 0.2.0 is installed, the loaded 0.1.0 kept until the next start".into(),
                id_dir.join("0.2.0/manifest.toml").is_file() && id_dir.join("0.1.0").is_dir(),
            ));
            c.push(("5: no panic".into(), !client.contains("PANIC") && !log.contains("PANIC")));
        } else {
            c.push(("5: the server answered".into(), false));
        }

        // 6: op 5's card restarts through the launcher; the approved offer is `have`.
        begin("port=update\n");
        crate::say!("== {}: op 5's card and have (port {})", root.name, port + 1);
        let settings = format!("{dev}\nsv_client_version \">=99.0.0\"\nsv_plugin_check_time 60");
        if let Some(server) = self.version_server(root, port + 1, &settings) {
            let client = self.fwgs_client_script_env(root, fwgs, port + 1, "tspkg-client-port", &middle, &env);
            let log = self.version_server_stop(root, server);
            let s = shown();
            let fake = fs::read_to_string(&fake_log).unwrap_or_default();
            c.push((
                "6: the superseded 0.1.0 went at start, and 0.2.0 loaded".into(),
                !client_dir.join("cl_plugins").join(OFFER).join("0.1.0").exists() && client.contains(&format!("loaded tsmod_{OFFER}{sfx}")),
            ));
            c.push(("6: the approved offer answers have".into(), log.contains(&format!(": {OFFER} 0.2.0 have"))));
            c.push(("6: tspkg list names it".into(), log.contains(&format!("plugins: {OFFER}@0.2.0"))));
            c.push((
                "6: the port card was shown".into(),
                s.contains("kind=port id=tsrs") && s.contains(">=99.0.0") && s.contains("answer=update"),
            ));
            c.push(("6: the client answered tsrs restart".into(), log.lines().any(|l| l.contains(": tsrs ") && l.contains(" restart"))));
            c.push((
                "6: relaunch started ts-launcher-fake with --reason port --port-version >=99.0.0".into(),
                ["arg relaunch", "arg --reason", "arg port", "arg --port-version", "arg >=99.0.0"].iter().all(|w| fake.contains(w)),
            ));
            c.push(("6: no panic".into(), !client.contains("PANIC") && !log.contains("PANIC")));
        } else {
            c.push(("6: the server answered".into(), false));
        }

        let _ = fs::remove_file(fake_exe.with_file_name("ts-launcher-fake.cfg"));
        let _ = fs::remove_file(gamedir.join("plugin_offers.toml"));
        remove_packages(&gamedir);
        let _ = fs::remove_dir(gamedir.join("plugins"));
        restore(&client_dir);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fastdl_serves_overrides_and_files() {
        let dir = std::env::temp_dir().join(format!("xtask-fastdl-{}", std::process::id()));
        fs::create_dir_all(dir.join("plugins")).unwrap();
        fs::write(dir.join("plugins/a.txt"), b"file").unwrap();
        let state = Mutex::new(FastDlState { root: dir.clone(), ..Default::default() });
        state.lock().unwrap().overrides.insert("/ts/plugins/b.txt".into(), b"over".to_vec());
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let get = |path: &str| {
            let mut s = std::net::TcpStream::connect(addr).unwrap();
            write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let (conn, _) = l.accept().unwrap();
            serve_one(conn, &state);
            let mut out = String::new();
            std::io::Read::read_to_string(&mut s, &mut out).unwrap();
            out
        };
        assert!(get("/ts/plugins/a.txt").ends_with("\r\n\r\nfile"));
        assert!(get("/ts/plugins/b.txt").ends_with("\r\n\r\nover"));
        assert!(get("/ts/../x").starts_with("HTTP/1.1 404"));
        assert!(get("/ts/plugins/missing").starts_with("HTTP/1.1 404"));
        assert_eq!(state.lock().unwrap().requests.len(), 4);
        let _ = fs::remove_dir_all(&dir);
    }
}
