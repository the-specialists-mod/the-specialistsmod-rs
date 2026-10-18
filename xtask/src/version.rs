//! `cargo xtask version [<root>...]`: the port client's version check at connect (issue #300,
//! `docs/implementation/client-plugins.md` §13.1) on each Linux root's dedicated server (TS 3.0
//! mode), with the port's FWGS client (`fwgs` root; the GoldSrc protocol for the Steam engines)
//! and, when present, the original TS 3.0 client (`~/games/xash_win_orig`, `TS_ORIG_CLIENT`):
//!
//! 1. `sv_client_version ">=99.0.0"`: the port's client is refused. The server logs
//!    `[ts] version: refused` with the `[tsrs:>=99.0.0]` tag, and the client shows the reason.
//! 2. `sv_client_version exact` and `sv_client_require_port 1`: the port's client joins (its
//!    version is the server's), and prints no version difference.
//! 3. The same settings: the original client is refused with the `[tsrs:=<version>]` tag
//!    (Steam engines only, as `xtask ts21`'s original-client check).
//!
//! The servers run one after another (a client per server), about 1.5 min per root.

use crate::a2s;
use crate::roots::{EXEC_SERVER_CFG, Engine, Os, Root};
use crate::{Ctx, say};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const SERVER_CFG: &str = "xtask_version.cfg";
const CLIENT_CFG: &str = "xtask_version_client.cfg";
/// A requirement no build meets.
const UNMET: &str = ">=99.0.0";

pub(crate) fn server_cfg(settings: &str) -> String {
    format!(
        "// Written by cargo xtask version.\n{EXEC_SERVER_CFG}developer 1\nmp_timelimit 0\nmp_fraglimit 0\nts_bot_quota 0\n{settings}\n"
    )
}

/// The workspace version, which the server and client libraries report (xtask shares it).
const PORT_VERSION: &str = env!("CARGO_PKG_VERSION");

impl Ctx {
    pub fn version_all(&self, roots: &[Root]) -> bool {
        let fwgs = Root::all().into_iter().find(|r| r.name == "fwgs").unwrap();
        if !fwgs.exists() {
            say!("== version: skipped (no fwgs root for the port's client)");
            return true;
        }
        let orig = std::env::var_os("TS_ORIG_CLIENT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join("games/xash_win_orig"));
        self.install(&fwgs);
        let mut ok = true;
        for (i, r) in roots.iter().filter(|r| r.os == Os::Linux && r.exists()).enumerate() {
            if r.name != fwgs.name {
                self.install(r);
            }
            let port = 27815 + 10 * i as u16;
            ok &= self.version_root(r, &fwgs, &orig, port);
        }
        say!("== version: {}", crate::pass(ok));
        ok
    }

    fn version_root(&self, root: &Root, fwgs: &Root, orig: &Path, port: u16) -> bool {
        let mut checks: Vec<(String, bool)> = Vec::new();

        say!("== {}: sv_client_version \"{UNMET}\" refuses the port's client (port {port})", root.name);
        if let Some(server) = self.version_server(root, port, &format!("sv_client_version \"{UNMET}\"")) {
            let client = self.version_fwgs_client(root, fwgs, port, "refused");
            let log = self.version_server_stop(root, server);
            let tag = format!("[tsrs:{UNMET}]");
            checks.push((
                "1: the server logs \"[ts] version: refused\" with the tag".into(),
                log.contains("[ts] version: refused") && log.contains(&tag),
            ));
            checks.push((format!("1: the client shows the reason ({tag})"), client.contains(&tag)));
            checks.push(("1: the client didn't enter the game".into(), !log.contains("entered the game")));
        } else {
            checks.push(("1: the server answered".into(), false));
        }

        say!("== {}: sv_client_version exact, sv_client_require_port 1 (port {})", root.name, port + 1);
        let strict = "sv_client_version exact\nsv_client_require_port 1";
        if let Some(server) = self.version_server(root, port + 1, strict) {
            let client = self.version_fwgs_client(root, fwgs, port + 1, "exact");
            let log = self.version_server_stop(root, server);
            checks.push(("2: the server didn't refuse the port's client".into(), !log.contains("[ts] version: refused")));
            checks.push(("2: the port's client joined".into(), client.contains("client connected") || log.contains("entered the game")));
            checks.push(("2: the client prints no version difference".into(), !client.contains("[ts] version: this server runs")));
            checks.push(("2: no panic".into(), !client.contains("PANIC") && !log.contains("PANIC")));
        } else {
            checks.push(("2: the server answered".into(), false));
        }

        if root.engine == Engine::Fwgs {
            say!("== {}: the original TS 3.0 client: skipped (FWGS servers, as xtask ts21)", root.name);
        } else if !orig.join("xash3d.exe").exists() {
            say!("== {}: the original TS 3.0 client: skipped ({} missing)", root.name, orig.display());
        } else {
            say!("== {}: sv_client_require_port 1 refuses the original TS 3.0 client (port {})", root.name, port + 2);
            if let Some(server) = self.version_server(root, port + 2, strict) {
                let client = self.version_orig_client(root, orig, port + 2);
                let log = self.version_server_stop(root, server);
                let tag = format!("[tsrs:={PORT_VERSION}]");
                checks.push((
                    "3: the server logs \"[ts] version: refused\" (Other)".into(),
                    log.contains("[ts] version: refused") && log.contains("(Other)"),
                ));
                checks.push((format!("3: the original client shows the reason ({tag})"), client.contains(&tag)));
            } else {
                checks.push(("3: the server answered".into(), false));
            }
        }

        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        let ok = checks.iter().all(|(_, ok)| *ok);
        say!("   {}: {}", root.name, crate::pass(ok));
        ok
    }

    /// Starts `root`'s dedicated server in TS 3.0 mode with `settings`; returns once it answers.
    pub(crate) fn version_server(&self, root: &Root, port: u16, settings: &str) -> Option<Child> {
        fs::write(root.path.join("ts").join(SERVER_CFG), server_cfg(settings)).unwrap();
        let _ = fs::remove_file(root.log_path());
        let mut cmd = root.server_command_with("ts_dojo", port, SERVER_CFG, 0);
        let out = fs::File::create(self.repo.join("target").join(format!("server-{}.log", root.name))).unwrap();
        cmd.stdout(out.try_clone().unwrap()).stderr(out);
        let mut child = cmd.spawn().expect("start server");
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(500));
            if a2s::info("127.0.0.1", port).is_ok() {
                std::thread::sleep(Duration::from_secs(3));
                return Some(child);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        say!("   FAIL: the server didn't answer");
        None
    }

    pub(crate) fn version_server_stop(&self, root: &Root, mut child: Child) -> String {
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(root.path.join("ts").join(SERVER_CFG));
        root.cleanup();
        root.read_log(&self.repo)
    }

    /// The port's client (`fwgs`) connects to `root`'s server, waits, and quits; returns its log.
    fn version_fwgs_client(&self, root: &Root, fwgs: &Root, port: u16, label: &str) -> String {
        self.fwgs_client_script(root, fwgs, port, label, "")
    }

    /// [`Self::version_fwgs_client`], running `middle` (console lines) halfway through the wait.
    pub(crate) fn fwgs_client_script(&self, root: &Root, fwgs: &Root, port: u16, label: &str, middle: &str) -> String {
        self.fwgs_client_script_env(root, fwgs, port, label, middle, &[])
    }

    /// [`Self::fwgs_client_script`] with `env` added to the client's environment.
    pub(crate) fn fwgs_client_script_env(
        &self,
        root: &Root,
        fwgs: &Root,
        port: u16,
        label: &str,
        middle: &str,
        env: &[(&str, String)],
    ) -> String {
        let w = "wait;".repeat(20);
        let proto = if root.engine == Engine::Fwgs { "" } else { " gs" };
        let half = format!("{w}\n").repeat(20);
        let cfg = format!("connect 127.0.0.1:{port}{proto}\n{half}{middle}{half}echo XTASK: done\nquit\n");
        let gamedir = fwgs.path.join("ts");
        fs::write(gamedir.join(CLIENT_CFG), cfg).unwrap();
        let log_path = self.repo.join("target").join(format!("version-client-{}-{label}.log", root.name));
        let log = fs::File::create(&log_path).unwrap();
        let mut client = Command::new(fwgs.path.join("xash3d"))
            .current_dir(&fwgs.path)
            .args(["-game", "ts", "-dev", "2", "-windowed", "-width", "800", "-height", "600", "+exec", CLIENT_CFG])
            .envs(env.iter().map(|(k, v)| (*k, v)))
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start client");
        let t = Instant::now();
        while client.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = client.kill();
        let _ = client.wait();
        let _ = fs::remove_file(gamedir.join(CLIENT_CFG));
        say!("   client log: {}", log_path.display());
        String::from_utf8_lossy(&fs::read(&log_path).unwrap_or_default()).replace('\r', "")
    }

    /// The original TS 3.0 client (FWGS 0.21 win32 with TS's `client.dll`, under Wine) connects
    /// with the GoldSrc protocol; returns its engine log (the root's own log is left as it was).
    pub(crate) fn version_orig_client(&self, root: &Root, orig: &Path, port: u16) -> String {
        let w = "wait;".repeat(20);
        let cfg_path = orig.join("ts").join(CLIENT_CFG);
        fs::write(&cfg_path, format!("connect 127.0.0.1:{port} gs\n{}quit\n", format!("{w}\n").repeat(30))).unwrap();
        let engine_log = orig.join("engine.log");
        let saved = fs::read(&engine_log).ok();
        let _ = fs::remove_file(&engine_log);
        let mut client = Command::new("wine")
            .current_dir(orig)
            .env("WINEPREFIX", crate::roots::wineprefix())
            .env("WINEDEBUG", "-all")
            .args(["xash3d.exe", "-game", "ts", "-dev", "2", "-log", "-windowed", "-width", "800", "-height", "600", "+exec", CLIENT_CFG])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the original client");
        let t = Instant::now();
        while client.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = client.kill();
        let _ = client.wait();
        crate::roots::wine_cleanup();
        let _ = fs::remove_file(&cfg_path);
        let text = String::from_utf8_lossy(&fs::read(&engine_log).unwrap_or_default()).replace('\r', "");
        let dest = self.repo.join("target").join(format!("version-orig-client-{}.log", root.name));
        let _ = fs::write(&dest, &text);
        match saved {
            Some(b) => {
                let _ = fs::write(&engine_log, b);
            }
            None => {
                let _ = fs::remove_file(&engine_log);
            }
        }
        say!("   client log: {}", dest.display());
        text
    }
}
