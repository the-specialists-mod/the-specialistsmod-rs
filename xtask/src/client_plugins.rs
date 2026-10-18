//! `cargo xtask client-plugins [fwgs fwgs-win ...]`: the client extension loader and API (issue
//! #293, docs/implementation/client-ext-api.md §9) on each FWGS root's listen server. It installs
//! the stub extension `tsmod_test` (crates/tsmod-client-test) in `cl_dlls/`, with fixtures the
//! loader must skip with a line on the console: another API version, a bad descriptor size, a
//! refusal, a library without the export, a library the approval gate doesn't approve, the other
//! architecture, junk and another platform's name. The development override
//! (`TSRS_DEV_CLIENT_EXT`, honoured by debug builds only) stands in for the approval store.
//!
//! The script joins ts_dojo, presses the stub's key (`ts_dev_key`), `+use` and `+attack`, puts a
//! stopped bot in front of the host, starts the stub's helper through `verified_spawn`, suspends
//! and resumes the extension (`ts_dev_extsuspend`) and kills the host. The stub records every
//! check of `tsmod_client_test::CHECKS` and writes them to its data directory's `report.txt`,
//! which this reads with the helper's `spawned.txt` (the engine log can sit in a buffer under
//! Wine). The fixtures are removed afterwards (and by every `install`).

use crate::roots::{Os, Root, Target};
use crate::{Ctx, say, secs};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::time::Duration;

/// The fixtures' names (`<name>` of `tsmod_<name>`), besides `test` itself.
const FIXTURES: [&str; 8] = ["test_badver", "test_badsize", "test_refuse", "unapproved", "noexport", "wrongarch", "junk", "otherplat"];

/// What the development override approves: the stub and the fixtures that must get as far as
/// their load function (`unapproved` must not).
const APPROVED: &str = "test,test_badver,test_badsize,test_refuse,noexport";

/// The extension library suffix of a build target (`ts_client_ext_api::library_suffix`).
pub fn suffix(t: &Target) -> &'static str {
    ts_client_ext_api::library_suffix(t.os == Os::Windows, t.triple.starts_with("x86_64"))
}

fn helper_file(t: &Target) -> &'static str {
    if t.os == Os::Windows { "tsmod_test_helper.exe" } else { "tsmod_test_helper" }
}

/// Removes every file xtask may have put in `cl_dlls/` (a run that was cut short).
pub fn remove_fixtures(cl_dlls: &Path) {
    let Ok(rd) = fs::read_dir(cl_dlls) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if std::iter::once("test").chain(FIXTURES).any(|f| name.starts_with(&format!("tsmod_{f}"))) {
            let _ = fs::remove_file(e.path());
        }
    }
}

impl Ctx {
    fn client_stub_dir(&self, t: &Target) -> PathBuf {
        self.repo.join("target").join(t.triple).join(self.profile())
    }

    /// `libtsmod_client_test.so` / `tsmod_client_test.dll` for `t`.
    fn client_stub(&self, t: &Target) -> PathBuf {
        self.client_stub_dir(t).join(if t.os == Os::Windows { "tsmod_client_test.dll" } else { "libtsmod_client_test.so" })
    }

    pub fn client_plugins_all(&self, roots: &[Root]) -> bool {
        if self.profile != crate::Profile::Dev {
            say!("== client-plugins: needs the dev profile (the approval override is compiled into debug builds only)");
            return false;
        }
        // The wrong-architecture fixture needs the other architecture's build too; the server
        // stub plugin answers the client stub's `TSExt` echo.
        for t in Target::ALL.iter().filter(|t| roots.iter().any(|r| r.target().os == t.os)) {
            let mut c = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
            c.current_dir(&self.repo).args(["build", "-p", "tsmod-client-test", "-p", "tsmod-test", "--target", t.triple]);
            if !c.status().expect("cargo").success() {
                exit(1);
            }
        }
        self.parallel(roots, |r, i| self.client_plugins(r, 27615 + 10 * i as u16))
    }

    /// Puts the stub, its helper and the fixtures into `<root>/ts/cl_dlls`.
    fn install_client_fixtures(&self, t: &Target, cl_dlls: &Path) {
        let sfx = suffix(t);
        let stub = self.client_stub(t);
        for name in ["test", "test_badver", "test_badsize", "test_refuse", "unapproved"] {
            crate::copy(&stub, &cl_dlls.join(format!("tsmod_{name}{sfx}")));
        }
        // A real library without the export: a copy of the client library itself.
        crate::copy(&cl_dlls.join(t.client_name), &cl_dlls.join(format!("tsmod_noexport{sfx}")));
        let other = *Target::ALL.iter().find(|o| o.os == t.os && o.triple != t.triple).expect("two architectures per OS");
        crate::copy(&self.client_stub(&other), &cl_dlls.join(format!("tsmod_wrongarch{sfx}")));
        fs::write(cl_dlls.join(format!("tsmod_junk{sfx}")), b"not a library").unwrap();
        let other_os = Target::ALL.iter().find(|o| o.os != t.os).expect("two platforms");
        fs::write(cl_dlls.join(format!("tsmod_otherplat{}", suffix(other_os))), b"skipped by name").unwrap();
        let helper =
            self.client_stub_dir(t).join(if t.os == Os::Windows { "tsmod-client-test-helper.exe" } else { "tsmod-client-test-helper" });
        crate::copy(&helper, &cl_dlls.join(helper_file(t)));
        fs::write(cl_dlls.join("tsmod_test.files"), format!("# Written by cargo xtask client-plugins.\n{}\n", helper_file(t))).unwrap();
    }

    fn client_plugins(&self, root: &Root, port: u16) -> bool {
        let t = root.target();
        let gamedir = root.path.join("ts");
        let cl_dlls = gamedir.join("cl_dlls");
        say!("== {}: client extensions on ts_dojo (listen server, port {port})", root.name);
        self.install_client_fixtures(&t, &cl_dlls);
        // The server stub plugin: the other end of the `TSExt` checks (`tsext_echo`).
        let dlls = gamedir.join("dlls");
        crate::copy(&self.test_plugin_artifact(&t), &dlls.join(format!("tsmod_test{}", crate::plugins::suffix(&t))));
        let data = gamedir.join("cl_plugins_data").join("test");
        let _ = fs::remove_dir_all(&data);
        let cfg = script();
        let run = self.run_script_env(
            root,
            "client_plugins",
            &cfg,
            port,
            Duration::from_secs(2),
            Duration::from_secs(300),
            &[("TSRS_DEV_CLIENT_EXT", APPROVED)],
        );
        remove_fixtures(&cl_dlls);
        crate::plugins::remove_fixtures(&dlls);
        let report = fs::read_to_string(data.join("report.txt")).unwrap_or_default();
        let spawned = fs::read_to_string(data.join("spawned.txt")).unwrap_or_default();
        for line in run.text.lines().filter(|l| l.contains("[ts] client extensions") || l.contains("[tsmod_") || l.contains("[ts ext]")) {
            say!("   | {}", line.trim_end());
        }
        let mut checks = client_plugin_checks(&run.text, &t, &report, &spawned);
        checks.insert(0, ("the script finished".into(), run.done && !run.exited));
        self.report(root, "client_plugins", &run, &checks.iter().map(|(w, ok)| (w.as_str(), *ok)).collect::<Vec<_>>())
    }
}

/// The listen-server script (see the module docs).
fn script() -> String {
    let mut cfg = crate::script_start("ts_dojo") + &crate::script_join_and_spawn();
    cfg += "tsext_test_cvar 5\n";
    cfg += &format!("ts_dev_key {k} 1\nwait;wait\nts_dev_key {k} 0\n{}", secs(0.5), k = tsmod_client_test::TEST_KEY);
    cfg += &format!("+use\nwait;wait;wait\n-use\n{}", secs(0.5));
    cfg += &format!("+attack\nwait;wait;wait\n-attack\n{}", secs(0.5));
    cfg += &format!("ts_bot_stop 1\nts_bot_quota 1\n{}", secs(8.0));
    cfg += &format!("ts_tp 2 0 -1200 60 270 0\n{}ts_tp 1 0 -1344 60 90 0\n{}ts_face 1 2\n{}", secs(0.5), secs(1.0), secs(0.5));
    // Minor 1: `TSExt` both ways (with the bot in, for the server's "not a port client"), the
    // command hooks on the port's `special` and its `_special`, the chat hooks.
    cfg += &format!("ts_dev_extlog 1\necho XTASK: tsext\ntsext_test_tsext\n{}", secs(1.0));
    for arm in ["block", "pass", "rename"] {
        cfg += &format!("tsext_test_arm {arm}\nspecial\n{}", secs(0.5));
    }
    cfg += "tsext_test_arm none\n";
    cfg += &format!("say tsext_block_me\n{}say tsext_hold_me\n{}say tsext_pass_me\n{}", secs(1.0), secs(2.0), secs(1.0));
    cfg += &format!("echo XTASK: spawn\ntsext_test_spawn\n{}", secs(1.0));
    cfg += &format!("echo XTASK: suspend\nts_dev_extsuspend test 1\n{}tsext_test_report\n{}", secs(1.0), secs(0.5));
    cfg += &format!("ts_dev_extsuspend test 0\n{}", secs(0.5));
    cfg += &format!("echo XTASK: report\ntsext_test_report\n{}", secs(0.5));
    cfg += &format!("echo XTASK: kill\nkill\n{}", secs(2.0));
    cfg += "tsext_test_report\nts_dev_extlist\nts_bot_quota 0\n";
    cfg
}

/// The checks on a run's log, the stub's report and the helper's marker.
pub fn client_plugin_checks(text: &str, t: &Target, report: &str, spawned: &str) -> Vec<(String, bool)> {
    let sfx = suffix(t);
    let other_os = Target::ALL.iter().find(|o| o.os != t.os).expect("two platforms");
    let line =
        |file: String, what: &str| text.lines().any(|l| l.contains("[ts] client extensions:") && l.contains(&file) && l.contains(what));
    // A `ts_dev_extlog` line with both texts.
    let ext = |what: &str, with: &str| text.lines().any(|l| l.contains("[ts ext] ") && l.contains(what) && l.contains(with));
    let mut c: Vec<(String, bool)> = vec![
        ("the development override announced".into(), text.contains("TSRS_DEV_CLIENT_EXT: approving")),
        ("tsmod_test loaded".into(), line(format!("loaded tsmod_test{sfx}"), "manual:test")),
        ("another API version skipped".into(), line(format!("tsmod_test_badver{sfx}"), "built for client extension API version")),
        ("a bad descriptor size skipped".into(), line(format!("tsmod_test_badsize{sfx}"), "descriptor")),
        ("a refusal skipped".into(), line(format!("tsmod_test_refuse{sfx}"), "refused")),
        ("a library without the export skipped".into(), line(format!("tsmod_noexport{sfx}"), "TSRS_ClientExtLoad export")),
        ("an unapproved library not loaded".into(), line(format!("tsmod_unapproved{sfx}"), "not approved")),
        ("the other architecture skipped".into(), line(format!("tsmod_wrongarch{sfx}"), "built for")),
        ("junk skipped".into(), line(format!("tsmod_junk{sfx}"), "not a library")),
        ("another platform's name skipped".into(), line(format!("tsmod_otherplat{}", suffix(other_os)), "library (this client is")),
        ("the unapproved library's code never ran".into(), !text.contains("which the approval gate must never let load")),
        (
            "the extension suspended and resumed".into(),
            text.contains("client extensions: test suspended") && text.contains("client extensions: test resumed"),
        ),
        ("the helper process ran".into(), spawned.contains("helper ran: from-tsmod_test")),
        ("the stub's report written".into(), !report.is_empty()),
        ("the extension's command reached the server plugin".into(), text.contains("[tsmod_test] tsext_echo to #1: echo:hello")),
        ("a blocked SayText not displayed".into(), ext("SayText blocked or held", "tsext_block_me") && !ext("released:", "tsext_block_me")),
        (
            "a held SayText displayed on release".into(),
            ext("SayText blocked or held", "tsext_hold_me") && ext("SayText released:", "tsext_hold_me"),
        ),
        ("an untouched SayText passed".into(), ext("SayText passed", "tsext_pass_me")),
        (
            "a DeathMsg rewritten in the kill feed".into(),
            ext("DeathMsg rewritten:", "tsext_rewritten") && text.contains("with tsext_rewritten"),
        ),
        ("a client command blocked".into(), ext("command special blocked", "")),
        ("a server command passed".into(), ext("server command \"_special\" sent as \"_special\"", "")),
        (
            "a renamed server command refused".into(),
            ext("server command \"_special\" blocked (a bad replacement)", "") && text.contains("changes the command's name"),
        ),
        ("no panic or crash".into(), !(text.contains("PANIC") || text.contains("FATAL ERROR") || text.contains("Segmentation"))),
    ];
    for what in tsmod_client_test::CHECKS {
        let ok = report.lines().any(|l| l.trim_end() == format!("check {what} ok"));
        c.push((format!("check {what}"), ok));
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_and_suffixes() {
        assert_eq!(suffix(&Target::WINDOWS_I686), ".dll");
        assert_eq!(suffix(&Target::LINUX_X86_64), "_amd64.so");
        let dir = std::env::temp_dir().join(format!("xtask-cl-fixtures-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for f in
            ["tsmod_test_amd64.so", "tsmod_test.files", "tsmod_test_helper", "tsmod_junk.dll", "tsmod_webview_amd64.so", "client_amd64.so"]
        {
            fs::write(dir.join(f), b"x").unwrap();
        }
        remove_fixtures(&dir);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(left, ["client_amd64.so", "tsmod_webview_amd64.so"], "only xtask's own files go");
    }

    #[test]
    fn a_passing_run() {
        let t = Target::LINUX_X86_64;
        let mut log = String::from("[ts] client extensions: TSRS_DEV_CLIENT_EXT: approving test (debug build only)\n");
        for l in [
            "loaded tsmod_test_amd64.so (tsmod_test (client stub) 1; manual:test, approved by x)",
            "tsmod_test_badver_amd64.so: built for client extension API version 2, this client has version 1, skipped",
            "tsmod_test_badsize_amd64.so: its extension descriptor is 8 bytes, this client's is 200, skipped",
            "tsmod_test_refuse_amd64.so: its TSRS_ClientExtLoad refused to load (-2), skipped",
            "tsmod_noexport_amd64.so: has no TSRS_ClientExtLoad export, skipped",
            "tsmod_unapproved_amd64.so: not approved (the approval store isn't installed in this build), skipped",
            "tsmod_wrongarch_amd64.so: built for linux-i386 (this client is linux-amd64), skipped",
            "tsmod_junk_amd64.so: not a library for this client (linux-amd64), skipped",
            "tsmod_otherplat.dll: a win32 library (this client is linux-amd64), skipped",
            "test suspended",
            "test resumed",
        ] {
            log += &format!("[ts] client extensions: {l}\n");
        }
        for l in [
            "SayText blocked or held (came as: Host: tsext_block_me)",
            "SayText blocked or held (came as: Host: tsext_hold_me)",
            "SayText released: Host: tsext_hold_me",
            "SayText passed (came as: Host: tsext_pass_me)",
            "DeathMsg rewritten: tsext_rewritten (came as: world)",
            "command special blocked",
            "server command \"_special\" sent as \"_special\"",
            "server command \"_special\" blocked (a bad replacement)",
        ] {
            log += &format!("[ts ext] {l}\n");
        }
        log +=
            "[tsmod_test] tsext_echo to #1: echo:hello bot=-3 all=1 big=-2 badch=-2 ver=3.0.8 (1)\nHost killed self with tsext_rewritten\n";
        log += "[tsmod_test] warning: its replacement for server command _special changes the command's name: blocked\n";
        let report: String = tsmod_client_test::CHECKS.iter().map(|c| format!("check {c} ok\n")).collect();
        let checks = client_plugin_checks(&log, &t, &report, "helper ran: from-tsmod_test\n");
        assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
        let bad = client_plugin_checks(&log, &t, &report.replace("check aim ok", "check aim FAIL x"), "");
        assert!(bad.iter().any(|(w, ok)| w == "check aim" && !ok));
        assert!(bad.iter().any(|(w, ok)| w == "the helper process ran" && !ok));
    }
}
