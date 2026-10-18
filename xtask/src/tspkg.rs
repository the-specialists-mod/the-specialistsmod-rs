//! Server offers of client plugins (issue #298, `docs/implementation/client-plugins.md` §6, §11,
//! §12, §13.1):
//!
//! * `cargo xtask plugin-offer [--required] <tspkg>` checks a built package as the server will
//!   (its entries and every file against its manifest, the signature), writes the files a client
//!   fetches before the package (`<base>.manifest.toml`, and `.manifest.sig` /
//!   `.publisher.cert` when the package has them) beside it, and prints its
//!   `plugin_offers.toml` entry, with the URL relative to `sv_downloadurl` (the package's path
//!   under `ts/`, as FastDL serves it).
//! * `cargo xtask tspkg [<root>...]` runs each Linux root's dedicated server (TS 3.0 mode) with
//!   the port's FWGS client (`fwgs` root), one server at a time:
//!   1. Offers, with `ts_dev_tspkg 1` (dev builds report the last release's version, below
//!      `TSPKG_SINCE`) and `sv_plugin_required_kick 1`: the server offers a signed stub package
//!      and refuses an offer whose hash doesn't match; the client prints the offer with its URL
//!      built from `sv_downloadurl`; its `tspkg ... offered` is logged, and `denied` to the
//!      required offer kicks it with the plugin named. The stub server plugin `tsmod_test` is
//!      installed for this part: the client's `tsmod_test_offer` makes it offer through the plugin
//!      API (`offer_client_plugin`), and it logs the `client_plugin_state` and
//!      `client_plugin_list` events with what `client_plugin_version` says (issue #321).
//!   2. Op 5: with `sv_client_version ">=99.0.0"` the port client is let in, told the version
//!      by `TSPkg`, and kicked after `sv_plugin_check_time 3` with the `[tsrs:...]` tag.
//!   3. Without `ts_dev_tspkg`, the same offers reach no client (this build is below
//!      `TSPKG_SINCE`, as an older port client would be); and on the Steam engines the original
//!      TS 3.0 client (`TS_ORIG_CLIENT`, as `xtask version`) gets no `TSPkg` and joins. Both run
//!      with `unlisted = "kick"` and a `manual:*` kick block, and neither is kicked (#302).
//!   4. to 6. The client's side (issue #299): offers, downloads over a FastDL stand-in, the
//!      cards, tampered packages, an update and op 5's card through the fake launcher
//!      ([`crate::tspkg_client`]). The client is the e2e build (`--features e2e`) for the whole
//!      run; the normal one is put back afterwards.
//!   7. to 10. Enforcement (issue #302): a `[[block]]` that suspends (resumed on leaving) and
//!      one that kicks, a required `match` with op 3 and the `sv_plugin_download_time` kick, and
//!      `unlisted = "suspend"` ([`crate::tspkg_client`]).

use crate::roots::{Engine, Os, Root};
use crate::tspkg_client::FastDl;
use crate::{Ctx, say};
use plugin_pkg::{Package, PublicKey, Sha256, SigningKey, builtin_roots};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The stub package's id and version.
const STUB: &str = "xtask_stub";
const STUB_VERSION: &str = "0.1.0";
/// A bad offer: the stub's package under another id and with a wrong hash.
const BAD: &str = "xtask_bad";
const DOWNLOAD_URL: &str = "http://127.0.0.1:27999/ts";

/// The files a client fetches before the package, beside `<base>.tspkg` (the server's
/// `plugin_offers::check::SIBLINGS`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{}{suffix}", name.strip_suffix(".tspkg").unwrap_or(&name)))
}

/// `plugin-offer`: checks `path`, writes its siblings, and returns the `[[offer]]` entry.
pub fn plugin_offer(path: &Path, required: bool) -> Result<String, String> {
    if path.extension().is_none_or(|e| e != "tspkg") {
        return Err(format!("{} isn't a .tspkg", path.display()));
    }
    let shown = path.display();
    let mut p = Package::open(path).map_err(|e| format!("{shown}: {e}"))?;
    p.verify_contents().map_err(|e| format!("{shown}: {e}"))?;
    let signer = p.check_signature(&builtin_roots()).map_err(|e| format!("{shown}: {e}"))?;
    let (sha, size) = Sha256::of_file(path).map_err(|e| format!("{shown}: {e}"))?;
    let m = p.manifest();
    let expected = format!("{}-{}.tspkg", m.id, m.version);
    if path.file_name().is_none_or(|n| n.to_string_lossy() != expected) {
        eprintln!("plugin-offer: note: packages are named <id>-<version>.tspkg ({expected})");
    }
    for (suffix, bytes) in
        [(".manifest.toml", Some(p.manifest_bytes())), (".manifest.sig", p.signature_bytes()), (".publisher.cert", p.cert_bytes())]
    {
        let sib = sibling(path, suffix);
        match bytes {
            Some(b) if fs::read(&sib).ok().as_deref() != Some(b) => {
                fs::write(&sib, b).map_err(|e| format!("{}: {e}", sib.display()))?;
                eprintln!("plugin-offer: wrote {}", sib.display());
            }
            Some(_) => {}
            None if sib.exists() => return Err(format!("{} is there, but the package has no such file; remove it", sib.display())),
            None => {}
        }
    }
    let trust = match &signer {
        plugin_pkg::Signer::Unsigned => "unsigned".to_owned(),
        plugin_pkg::Signer::Signed { key, verified_as: Some(n) } => format!("verified ({n}, {})", key.fingerprint()),
        plugin_pkg::Signer::Signed { key, verified_as: None } => format!("signed ({})", key.fingerprint()),
    };
    // The URL relative to sv_downloadurl is the package's path under the game directory.
    let full = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let comps: Vec<String> = full.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    let url = match comps.iter().rposition(|c| c == "ts") {
        Some(i) if i + 1 < comps.len() => comps[i + 1..].join("/"),
        _ => format!("plugins/{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
    };
    if size > u64::from(u32::MAX) {
        return Err(format!("{shown} is {size} bytes, over TSPkg's 32-bit size"));
    }
    Ok(format!(
        "# {} {} ({}), {trust}\n[[offer]]\nid = \"{}\"\nversion = \"{}\"\nurl = \"{url}\"   # relative to sv_downloadurl, or an https:// URL with file = \"{url}\"\nsha256 = \"{sha}\"\nsize = {size}\nrequired = {required}\nnote = \"\"\n",
        m.name, m.version, m.id, m.id, m.version
    ))
}

/// Writes the stub package into `<gamedir>/plugins/` and returns its `[[offer]]` entries: the
/// stub (required) and the bad one.
fn write_stub(repo: &Path, gamedir: &Path) -> Result<String, String> {
    let src = repo.join("target").join("tspkg-stub");
    let _ = fs::remove_dir_all(&src);
    fs::create_dir_all(src.join("lib")).map_err(|e| e.to_string())?;
    let lib = b"not a real library: cargo xtask tspkg's stub package";
    fs::write(src.join("lib/tsmod_xtask_stub.so"), lib).map_err(|e| e.to_string())?;
    let key = SigningKey::from_bytes(&[42; 32]);
    let manifest = format!(
        "format = 1\nid = \"{STUB}\"\nname = \"xtask stub\"\nversion = \"{STUB_VERSION}\"\nhomepage = \"https://example.org/xtask-stub\"\n\
         client_ext_api = 1\n\n[publisher]\nname = \"xtask\"\nkey = \"{}\"\n\n[platforms]\nlinux-x86_64 = \"lib/tsmod_xtask_stub.so\"\n\n\
         [[files]]\npath = \"lib/tsmod_xtask_stub.so\"\nsize = {}\nsha256 = \"{}\"\n",
        PublicKey::of(&key),
        lib.len(),
        Sha256::of_bytes(lib)
    );
    let sig = plugin_pkg::sign_manifest(&key, manifest.as_bytes());
    let plugins = gamedir.join("plugins");
    fs::create_dir_all(&plugins).map_err(|e| e.to_string())?;
    let pkg = plugins.join(format!("{STUB}-{STUB_VERSION}.tspkg"));
    let file = fs::File::create(&pkg).map_err(|e| e.to_string())?;
    plugin_pkg::build::write_package(file, manifest.as_bytes(), Some(&sig), None, &src).map_err(|e| e.to_string())?;
    let entry = plugin_offer(&pkg, true)?;
    let (_, size) = Sha256::of_file(&pkg).map_err(|e| e.to_string())?;
    let bad = format!(
        "[[offer]]\nid = \"{BAD}\"\nversion = \"{STUB_VERSION}\"\nurl = \"plugins/{STUB}-{STUB_VERSION}.tspkg\"\nsha256 = \"{}\"\nsize = {size}\n",
        "0".repeat(64)
    );
    Ok(format!("{entry}\n{bad}"))
}

fn remove_stub(gamedir: &Path) {
    let _ = fs::remove_file(gamedir.join("plugin_offers.toml"));
    for suffix in [".tspkg", ".manifest.toml", ".manifest.sig", ".publisher.cert"] {
        let _ = fs::remove_file(gamedir.join("plugins").join(format!("{STUB}-{STUB_VERSION}{suffix}")));
    }
    let _ = fs::remove_dir(gamedir.join("plugins"));
}

impl Ctx {
    pub fn tspkg_all(&self, roots: &[Root]) -> bool {
        let fwgs = Root::all().into_iter().find(|r| r.name == "fwgs").unwrap();
        if !fwgs.exists() {
            say!("== tspkg: skipped (no fwgs root for the port's client)");
            return true;
        }
        let orig = std::env::var_os("TS_ORIG_CLIENT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join("games/xash_win_orig"));
        // The client's side (tspkg_client.rs): the e2e client (cards answered from a file, the
        // fake launcher beside it), the stub extension the packages carry, and FastDL.
        self.install_e2e(&fwgs);
        let t = fwgs.target();
        let mut c = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
        c.current_dir(&self.repo).args(["build", "-p", "tsmod-client-test", "--target", t.triple]);
        if self.profile != crate::Profile::Dev {
            c.arg("--release");
        }
        if !c.status().expect("cargo").success() {
            std::process::exit(1);
        }
        // The stub server plugin (part 1), for every Linux root's server library.
        let mut built = Vec::new();
        for t in roots.iter().filter(|r| r.os == Os::Linux && r.exists()).map(Root::target) {
            if built.contains(&t.triple) {
                continue;
            }
            built.push(t.triple);
            let mut c = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
            c.current_dir(&self.repo).args(["build", "-p", "tsmod-test", "--target", t.triple]);
            if self.profile != crate::Profile::Dev {
                c.arg("--release");
            }
            if !c.status().expect("cargo").success() {
                std::process::exit(1);
            }
        }
        let dl = match FastDl::start() {
            Ok(dl) => dl,
            Err(e) => {
                say!("== tspkg: FAIL: {e}");
                return false;
            }
        };
        let mut ok = true;
        for (i, r) in roots.iter().filter(|r| r.os == Os::Linux && r.exists()).enumerate() {
            if r.name != fwgs.name {
                self.install(r);
            }
            ok &= self.tspkg_root(r, &fwgs, &orig, 27915 + 10 * i as u16, &dl);
        }
        // Put the normal client back.
        self.install(&fwgs);
        say!("== tspkg: {}", crate::pass(ok));
        ok
    }

    fn tspkg_root(&self, root: &Root, fwgs: &Root, orig: &Path, port: u16, dl: &FastDl) -> bool {
        let gamedir = root.path.join("ts");
        dl.0.lock().unwrap().root = gamedir.clone();
        let mut checks: Vec<(String, bool)> = Vec::new();
        let offers = match write_stub(&self.repo, &gamedir) {
            Ok(o) => o,
            Err(e) => {
                say!("   FAIL: the stub package: {e}");
                return false;
            }
        };
        fs::write(gamedir.join("plugin_offers.toml"), &offers).unwrap();
        let url = format!("{DOWNLOAD_URL}/plugins/{STUB}-{STUB_VERSION}.tspkg");
        let dev = format!("sv_cheats 1\nts_dev_tspkg 1\nsv_downloadurl \"{DOWNLOAD_URL}\"");

        say!("== {}: offers and tspkg replies (port {port})", root.name);
        let dlls = gamedir.join("dlls");
        let t = root.target();
        crate::copy(&self.test_plugin_artifact(&t), &dlls.join(format!("tsmod_test{}", crate::plugins::suffix(&t))));
        let server = self.version_server(root, port, &format!("{dev}\nsv_plugin_required_kick 1"));
        if let Some(server) = server {
            let w = format!("{}\n", "wait;".repeat(20)).repeat(10);
            let middle = format!(
                "{w}cmd tsmod_test_offer {STUB}\n{w}cmd tspkg {STUB} {STUB_VERSION} offered\n{w}cmd tspkg list\n{w}\
                 cmd tspkg {STUB} {STUB_VERSION} denied\n{w}"
            );
            let client = self.fwgs_client_script(root, fwgs, port, "tspkg-offers", &middle);
            let log = self.version_server_stop(root, server);
            crate::plugins::remove_fixtures(&dlls);
            checks.push((
                format!("1: the server offers {STUB}"),
                log.contains(&format!("[ts] tspkg: offering {STUB} {STUB_VERSION} (signed")),
            ));
            checks.push((format!("1: the server refuses {BAD} (hash)"), log.contains(&format!("not offering {BAD} {STUB_VERSION}"))));
            checks.push(("1: the offer is sent to the port client".into(), log.contains(&format!("offered {STUB} {STUB_VERSION} to"))));
            checks.push((
                "1: the client prints the offer with its sv_downloadurl URL".into(),
                client.contains(&format!("the server offers the client plugin {STUB} {STUB_VERSION}")) && client.contains(&url),
            ));
            checks.push(("1: the client reads every TSPkg".into(), !client.contains("can't read") && !client.contains("UserMsg")));
            checks.push(("1: the server logs the client's \"offered\"".into(), log.contains(&format!(": {STUB} {STUB_VERSION} offered"))));
            checks.push((
                "1: \"denied\" to the required offer kicks, naming the plugin".into(),
                log.contains("kicking") && log.contains("requires the client plugin xtask stub 0.1.0 (https://example.org/xtask-stub)"),
            ));
            checks.push(("1: no panic".into(), !client.contains("PANIC") && !log.contains("PANIC")));
            checks.extend(plugin_api_checks(&log));
        } else {
            crate::plugins::remove_fixtures(&dlls);
            checks.push(("1: the server answered".into(), false));
        }

        say!("== {}: op 5 port_version (port {})", root.name, port + 1);
        if let Some(server) = self.version_server(root, port + 1, &format!("{dev}\nsv_client_version \">=99.0.0\"\nsv_plugin_check_time 3"))
        {
            let client = self.fwgs_client_script(root, fwgs, port + 1, "tspkg-port-version", "");
            let log = self.version_server_stop(root, server);
            checks.push(("2: the server lets the port client in to tell it".into(), log.contains("[ts] version: let in")));
            checks.push(("2: the server sends op 5".into(), log.contains("[ts] tspkg: port_version >=99.0.0 to")));
            checks.push((
                "2: the client prints the version the server needs".into(),
                client.contains("[ts] version: this server needs The Specialists port >=99.0.0"),
            ));
            checks.push(("2: the server kicks it with the tag".into(), log.contains("kicking") && log.contains("[tsrs:>=99.0.0]")));
        } else {
            checks.push(("2: the server answered".into(), false));
        }

        // From here the policy kicks every plugin it can: a client that doesn't hook `TSPkg` must
        // still join (#302: nothing is enforced on clients that can't be told).
        fs::write(
            gamedir.join("plugin_offers.toml"),
            format!("{offers}\n[policy]\nunlisted = \"kick\"\n\n[[block]]\nid = \"manual:*\"\nversions = \"*\"\naction = \"kick\"\n"),
        )
        .unwrap();
        say!("== {}: no TSPkg to clients that don't hook it (port {})", root.name, port + 2);
        if let Some(server) = self.version_server(root, port + 2, &format!("sv_downloadurl \"{DOWNLOAD_URL}\"")) {
            let client = self.fwgs_client_script(root, fwgs, port + 2, "tspkg-old", "");
            let log = self.version_server_stop(root, server);
            checks.push(("3: the server still checks the offer".into(), log.contains(&format!("offering {STUB} {STUB_VERSION}"))));
            checks.push((
                "3: a port client below TSPKG_SINCE gets no offer".into(),
                !log.contains(&format!("offered {STUB}")) && !client.contains("[ts] tspkg:"),
            ));
            checks.push(("3: it joins".into(), client.contains("client connected") || log.contains("entered the game")));
            checks.push(("3: unlisted = kick and a manual:* block leave it alone".into(), !log.contains("kicking")));
        } else {
            checks.push(("3: the server answered".into(), false));
        }
        if root.engine != Engine::Fwgs && orig.join("xash3d.exe").exists() {
            say!("== {}: the original TS 3.0 client gets no TSPkg (port {})", root.name, port + 3);
            if let Some(server) = self.version_server(root, port + 3, &dev) {
                let client = self.version_orig_client(root, orig, port + 3);
                let log = self.version_server_stop(root, server);
                checks.push(("3: the original client gets no offer".into(), !log.contains(&format!("offered {STUB}"))));
                checks.push(("3: the original client reports no unknown message".into(), !client.contains("UserMsg")));
                checks.push(("3: the original client joins".into(), log.contains("entered the game")));
                checks.push(("3: the policy doesn't kick the original client".into(), !log.contains("kicking")));
            } else {
                checks.push(("3: the server answered (original client)".into(), false));
            }
        }
        remove_stub(&gamedir);
        checks.extend(self.tspkg_client(root, fwgs, port + 4, dl, &dev));

        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        let ok = checks.iter().all(|(_, ok)| *ok);
        say!("   {}: {}", root.name, crate::pass(ok));
        ok
    }
}

/// Part 1's plugin API checks (issue #321) on the server's log: the stub plugin's lines.
fn plugin_api_checks(log: &str) -> Vec<(String, bool)> {
    let line = |a: &str, b: &str| log.lines().any(|l| l.contains("[tsmod_test] ") && l.contains(a) && l.contains(b));
    vec![
        ("1: offer_client_plugin offers through the plugin API".into(), line("offer_client_plugin #", &format!("{STUB}: 1 unknown=-2"))),
        (
            "1: client_plugin_state reports the client's \"offered\"".into(),
            line("client_plugin_state #", &format!("{STUB} {STUB_VERSION} offered")),
        ),
        (
            "1: client_plugin_state reports the refusal before the kick".into(),
            line("client_plugin_state #", &format!("{STUB} {STUB_VERSION} denied loaded=-")),
        ),
        ("1: client_plugin_list reports the complete list".into(), line("client_plugin_list #", "versions agree")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plugin_api_checks_read_the_stubs_lines() {
        let log = format!(
            "[tsmod_test] offer_client_plugin #1 {STUB}: 1 unknown=-2\n\
             [tsmod_test] client_plugin_state #1 {STUB} {STUB_VERSION} offered loaded=-\n\
             [tsmod_test] client_plugin_list #1: 0 [] versions agree\n\
             [tsmod_test] client_plugin_state #1 {STUB} {STUB_VERSION} denied loaded=-\n"
        );
        assert!(plugin_api_checks(&log).iter().all(|(_, ok)| *ok));
        let bad = log.replace("1 unknown", "0 unknown").replace("agree", "differ");
        assert_eq!(plugin_api_checks(&bad).iter().filter(|(_, ok)| !ok).count(), 2);
    }

    #[test]
    fn plugin_offer_writes_the_siblings_and_the_entry() {
        let dir = std::env::temp_dir().join(format!("xtask-plugin-offer-{}", std::process::id()));
        let gamedir = dir.join("ts");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&gamedir).unwrap();
        let repo = dir.clone();
        let entries = write_stub(&repo, &gamedir).unwrap();
        assert!(entries.contains(&format!("url = \"plugins/{STUB}-{STUB_VERSION}.tspkg\"")), "{entries}");
        assert!(entries.contains("required = true") && entries.contains(", signed ("), "{entries}");
        let plugins = gamedir.join("plugins");
        let pkg = plugins.join(format!("{STUB}-{STUB_VERSION}.tspkg"));
        let p = Package::open(&pkg).unwrap();
        assert_eq!(fs::read(sibling(&pkg, ".manifest.toml")).unwrap(), p.manifest_bytes());
        assert_eq!(fs::read(sibling(&pkg, ".manifest.sig")).unwrap(), p.signature_bytes().unwrap());
        assert!(!sibling(&pkg, ".publisher.cert").exists());
        fs::write(sibling(&pkg, ".publisher.cert"), b"x").unwrap();
        assert!(plugin_offer(&pkg, false).unwrap_err().contains("no such file"));
        remove_stub(&gamedir);
        assert!(!plugins.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
