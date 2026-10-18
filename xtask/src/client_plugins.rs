//! `cargo xtask client-plugins [fwgs fwgs-win ...]`: the client extension loader and API (issue
//! #293, docs/implementation/client-ext-api.md §9) and the approval store, start-up verification
//! and prompt (issue #297, client-plugins.md §11 e2e 1) on each FWGS root's listen server.
//!
//! It installs the stub extension `tsmod_test` (crates/tsmod-client-test) in `cl_dlls/`, with
//! fixtures the loader must skip with a line on the console: another API version, a bad
//! descriptor size, a refusal, a library without the export, a library nobody approved, the
//! other architecture, junk and another platform's name. They're approved the way a developer
//! does it, with `plugin-approve` into a fresh `cl_plugins/approved.toml` (the root's own store is
//! set aside and put back afterwards), except the refusal, which the development override
//! (`TSRS_DEV_CLIENT_EXT`, debug builds only) approves so that stays tested too.
//!
//! The approval fixtures, all quiet copies of the stub ([`QUIET`]): packages built here and
//! installed into `cl_plugins/` (one that loads and starts its helper through `verified_spawn`;
//! three approved and then tampered with, a changed byte, an extra file and a missing file;
//! one whose `[requires] port` this client doesn't meet; one approved for the listen server only,
//! which loads on connecting), and two new hand-installed libraries the test hook
//! (`TSRS_DEV_PLUGIN_PROMPT`, debug builds only) answers "Allow" and "Never" for. The hook
//! records every box in [`PROMPT_LOG`].
//!
//! The server stub plugin (`crates/tsmod-test`) goes into `dlls/` as the other end of the
//! extension API's `TSExt` checks (issue #294), and `ts_dev_extlog 1` logs the interception hooks'
//! decisions (issue #295) as `[ts ext]` lines, which the checks read.
//!
//! The script joins ts_dojo, presses the stub's key (`ts_dev_key`), `+use` and `+attack`, puts a
//! stopped bot in front of the host, sends `TSExt` both ways, arms the command hooks on `special`,
//! says the chat lines the stub blocks, holds and passes, starts the stub's helper through `verified_spawn`, suspends
//! and resumes the extension (`ts_dev_extsuspend`), kills the host and lists and re-verifies the
//! plugins. The stub records every check of `tsmod_client_test::CHECKS` and writes them to its data
//! directory's `report.txt`, which this reads with the helpers' `spawned.txt` (the engine log can
//! sit in a buffer under Wine). The fixtures are removed afterwards (and by every `install`).

use crate::roots::{Os, Root, Target};
use crate::{Ctx, say, secs};
use plugin_pkg::{Platform, Scope, State, Store};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::time::Duration;

/// The fixtures' names (`<name>` of `tsmod_<name>`), besides `test` itself.
const FIXTURES: [&str; 8] = ["test_badver", "test_badsize", "test_refuse", "unapproved", "noexport", "wrongarch", "junk", "otherplat"];

/// Approved with `plugin-approve`: the stub and the fixtures that must get as far as their load
/// function.
const STORE_APPROVED: [&str; 4] = ["test", "test_badver", "test_badsize", "noexport"];

/// What the development override approves (`unapproved` must not load).
const DEV_APPROVED: &str = "test_refuse";

/// The prefix of the approval fixtures (the stub loads quietly under such names).
const QUIET: &str = "quiet";

/// The test hook's answers to the start-up boxes.
const PROMPT_ANSWERS: &str = "manual:quiet_allow=allow,manual:quiet_never=never,*=later";

/// The test hook's record of the boxes, in the game directory.
const PROMPT_LOG: &str = "xtask_plugin_prompts.log";

/// The packages built for the run: (id, tampering, `[requires] port`).
const PACKAGES: [(&str, Tamper, Option<&str>); 6] = [
    ("quietpkg", Tamper::None, None),
    ("quiettamper_byte", Tamper::Byte, None),
    ("quiettamper_extra", Tamper::Extra, None),
    ("quiettamper_missing", Tamper::Missing, None),
    ("quietport", Tamper::None, Some("<0.1")),
    ("quietserver", Tamper::None, None),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tamper {
    None,
    Byte,
    Extra,
    Missing,
}

/// The extension library suffix of a build target (`ts_client_ext_api::library_suffix`).
pub fn suffix(t: &Target) -> &'static str {
    ts_client_ext_api::library_suffix(t.os == Os::Windows, t.triple.starts_with("x86_64"))
}

fn helper_file(t: &Target) -> &'static str {
    if t.os == Os::Windows { "tsmod_test_helper.exe" } else { "tsmod_test_helper" }
}

fn platform(t: &Target) -> Platform {
    t.platform.parse().expect("a client plugin platform")
}

/// Removes every file xtask may have put in `cl_dlls/` (a run that was cut short).
pub fn remove_fixtures(cl_dlls: &Path) {
    let Ok(rd) = fs::read_dir(cl_dlls) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if std::iter::once("test").chain(FIXTURES).chain([QUIET]).any(|f| name.starts_with(&format!("tsmod_{f}"))) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn store_path(gamedir: &Path) -> PathBuf {
    plugin_pkg::store_path(&gamedir.join("cl_plugins"))
}

/// The root's own store while a run uses its own (or a marker that there was none).
fn backup_paths(gamedir: &Path) -> (PathBuf, PathBuf) {
    let s = store_path(gamedir);
    (s.with_file_name("approved.toml.xtask-backup"), s.with_file_name("approved.toml.xtask-none"))
}

/// Sets the root's store aside for a run.
fn set_store_aside(gamedir: &Path) {
    remove_plugin_fixtures(gamedir);
    let (backup, none) = backup_paths(gamedir);
    fs::create_dir_all(gamedir.join("cl_plugins")).unwrap();
    if store_path(gamedir).exists() {
        fs::rename(store_path(gamedir), backup).unwrap();
    } else {
        fs::write(none, b"").unwrap();
    }
}

/// Removes the approval fixtures (`cl_plugins/quiet*`, their data, the prompt log) and puts the
/// root's own store back (a run that was cut short too).
pub fn remove_plugin_fixtures(gamedir: &Path) {
    let (backup, none) = backup_paths(gamedir);
    if backup.exists() {
        let _ = fs::rename(&backup, store_path(gamedir));
    } else if none.exists() {
        let _ = fs::remove_file(store_path(gamedir));
        let _ = fs::remove_file(&none);
    }
    for dir in [gamedir.join("cl_plugins"), gamedir.join("cl_plugins_data")] {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(QUIET) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
    let _ = fs::remove_file(gamedir.join(PROMPT_LOG));
}

impl Ctx {
    fn client_stub_dir(&self, t: &Target) -> PathBuf {
        self.repo.join("target").join(t.triple).join(self.profile())
    }

    /// `libtsmod_client_test.so` / `tsmod_client_test.dll` for `t`.
    pub(crate) fn client_stub(&self, t: &Target) -> PathBuf {
        self.client_stub_dir(t).join(if t.os == Os::Windows { "tsmod_client_test.dll" } else { "libtsmod_client_test.so" })
    }

    fn client_helper(&self, t: &Target) -> PathBuf {
        self.client_stub_dir(t).join(if t.os == Os::Windows { "tsmod-client-test-helper.exe" } else { "tsmod-client-test-helper" })
    }

    pub fn client_plugins_all(&self, roots: &[Root]) -> bool {
        if self.profile != crate::Profile::Dev {
            say!("== client-plugins: needs the dev profile (the approval override and prompt hook are compiled into debug builds only)");
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
        for name in ["test", "test_badver", "test_badsize", "test_refuse", "unapproved", "quiet_allow", "quiet_never"] {
            crate::copy(&stub, &cl_dlls.join(format!("tsmod_{name}{sfx}")));
        }
        // A real library without the export: a copy of the client library itself.
        crate::copy(&cl_dlls.join(t.client_name), &cl_dlls.join(format!("tsmod_noexport{sfx}")));
        let other = *Target::ALL.iter().find(|o| o.os == t.os && o.triple != t.triple).expect("two architectures per OS");
        crate::copy(&self.client_stub(&other), &cl_dlls.join(format!("tsmod_wrongarch{sfx}")));
        fs::write(cl_dlls.join(format!("tsmod_junk{sfx}")), b"not a library").unwrap();
        let other_os = Target::ALL.iter().find(|o| o.os != t.os).expect("two platforms");
        fs::write(cl_dlls.join(format!("tsmod_otherplat{}", suffix(other_os))), b"skipped by name").unwrap();
        crate::copy(&self.client_helper(t), &cl_dlls.join(helper_file(t)));
        fs::write(cl_dlls.join("tsmod_test.files"), format!("# Written by cargo xtask client-plugins.\n{}\n", helper_file(t))).unwrap();
    }

    /// Builds the approval packages ([`PACKAGES`]), installs and approves them, then tampers.
    fn install_plugin_packages(&self, t: &Target, gamedir: &Path) -> Result<(), String> {
        let plat = platform(t);
        let src_root = self.repo.join("target").join(format!("client-plugins-pkg-{}", t.platform));
        for (id, tamper, port) in PACKAGES {
            let src = src_root.join(id);
            let _ = fs::remove_dir_all(&src);
            let lib = format!("lib/{}/tsmod_{id}{}", plat, suffix(t));
            let helper = format!("bin/{}", helper_file(t));
            fs::create_dir_all(src.join(format!("lib/{plat}"))).unwrap();
            fs::create_dir_all(src.join("bin")).unwrap();
            fs::create_dir_all(src.join("data")).unwrap();
            crate::copy(&self.client_stub(t), &src.join(&lib));
            fs::write(src.join("data/readme.txt"), b"hello").unwrap();
            let mut paths = vec![lib.clone(), "data/readme.txt".to_string()];
            let with_helper = id == "quietpkg";
            if with_helper {
                crate::copy(&self.client_helper(t), &src.join(&helper));
                paths.push(helper.clone());
            }
            let mut m = format!(
                "format = 1\nid = \"{id}\"\nname = \"xtask {id}\"\nversion = \"0.1.0\"\nclient_ext_api = {}\n",
                ts_client_ext_api::VERSION
            );
            if let Some(req) = port {
                m += &format!("\n[requires]\nport = \"{req}\"\n");
            }
            m += &format!("\n[platforms]\n{plat} = \"{lib}\"\n");
            if with_helper {
                m += &format!("\n[declares]\nprocesses = [\"{helper}\"]\n");
            }
            for p in &paths {
                let f = plugin_pkg::build::file_entry(&src, p, None).map_err(|e| format!("{p}: {e}"))?;
                m += &format!("\n[[files]]\npath = \"{}\"\nsize = {}\nsha256 = \"{}\"\n", f.path, f.size, f.sha256);
            }
            let pkg = src_root.join(format!("{id}-0.1.0.tspkg"));
            plugin_pkg::build::write_package(fs::File::create(&pkg).unwrap(), m.as_bytes(), None, None, &src).map_err(|e| e.to_string())?;
            crate::plugin_approve::approve(&pkg, Some(gamedir), plat, |e| {
                if id == "quietserver" {
                    // The listen server, as the net API names it.
                    e.scope = Scope::Server;
                    e.servers = vec!["loopback".into()];
                }
            })?;
            let dir = gamedir.join("cl_plugins").join(id).join("0.1.0").join("files");
            match tamper {
                Tamper::None => {}
                Tamper::Byte => fs::write(dir.join("data/readme.txt"), b"hellp").unwrap(),
                Tamper::Extra => fs::write(dir.join("data/extra.txt"), b"x").unwrap(),
                Tamper::Missing => fs::remove_file(dir.join("data/readme.txt")).unwrap(),
            }
        }
        Ok(())
    }

    fn client_plugins(&self, root: &Root, port: u16) -> bool {
        let t = root.target();
        let gamedir = root.path.join("ts");
        let cl_dlls = gamedir.join("cl_dlls");
        say!("== {}: client extensions and plugin approval on ts_dojo (listen server, port {port})", root.name);
        self.install_client_fixtures(&t, &cl_dlls);
        // The server stub plugin: the other end of the `TSExt` checks (`tsext_echo`).
        let dlls = gamedir.join("dlls");
        crate::copy(&self.test_plugin_artifact(&t), &dlls.join(format!("tsmod_test{}", crate::plugins::suffix(&t))));
        set_store_aside(&gamedir);
        let mut setup = Ok(());
        for name in STORE_APPROVED {
            if setup.is_ok() {
                let lib = cl_dlls.join(format!("tsmod_{name}{}", suffix(&t)));
                setup = crate::plugin_approve::approve(&lib, Some(&gamedir), platform(&t), |_| {}).map(|_| ());
            }
        }
        if setup.is_ok() {
            setup = self.install_plugin_packages(&t, &gamedir);
        }
        let data = gamedir.join("cl_plugins_data");
        let _ = fs::remove_dir_all(data.join("test"));
        // The MOTD the script's `motd` asks for (the takeover checks): `script_start` points
        // `motdfile` at a file that doesn't exist, so the script switches to this one meanwhile.
        let motd = gamedir.join(MOTD_FILE);
        let _ = fs::write(&motd, "tsext MOTD for the client extension takeover checks\n");
        let run = self.run_script_env(
            root,
            "client_plugins",
            &script(),
            port,
            Duration::from_secs(2),
            Duration::from_secs(300),
            &[
                ("TSRS_DEV_CLIENT_EXT", DEV_APPROVED),
                ("TSRS_DEV_PLUGIN_PROMPT", PROMPT_ANSWERS),
                ("TSRS_DEV_PLUGIN_PROMPT_LOG", PROMPT_LOG),
            ],
        );
        let report = fs::read_to_string(data.join("test/report.txt")).unwrap_or_default();
        let spawned = fs::read_to_string(data.join("test/spawned.txt")).unwrap_or_default();
        let pkg_spawned = fs::read_to_string(data.join("quietpkg/spawned.txt")).unwrap_or_default();
        let prompts = fs::read_to_string(gamedir.join(PROMPT_LOG)).unwrap_or_default();
        let store = Store::load(&store_path(&gamedir)).map(|(s, _)| s).unwrap_or_default();
        let _ = fs::remove_file(&motd);
        remove_fixtures(&cl_dlls);
        remove_plugin_fixtures(&gamedir);
        crate::plugins::remove_fixtures(&dlls);
        for line in run.text.lines().filter(|l| {
            l.contains("[ts] client extensions") || l.contains("[ts] client plugins") || l.contains("[tsmod_") || l.contains("[ts ext]")
        }) {
            say!("   | {}", line.trim_end());
        }
        let mut checks = client_plugin_checks(&run.text, &t, &report, &spawned);
        checks.extend(approval_checks(&run.text, &t, &prompts, &store, &pkg_spawned));
        checks.insert(0, ("the fixtures were approved with plugin-approve".into(), setup.as_ref().is_ok()));
        if let Err(e) = &setup {
            say!("   setup: {e}");
        }
        checks.insert(1, ("the script finished".into(), run.done && !run.exited));
        self.report(root, "client_plugins", &run, &checks.iter().map(|(w, ok)| (w.as_str(), *ok)).collect::<Vec<_>>())
    }
}

/// The MOTD file the script's `motd` sends (in the game directory, for the run only).
const MOTD_FILE: &str = "xtask_cp_motd.txt";

/// The listen-server script (see the module docs).
fn script() -> String {
    let mut cfg = crate::script_start("ts_dojo") + &crate::script_join_and_spawn();
    cfg += "tsext_test_cvar 5\n";
    cfg += &format!("ts_dev_key {k} 1\nwait;wait\nts_dev_key {k} 0\n{}", secs(0.5), k = tsmod_client_test::TEST_KEY);
    cfg += &format!("+use\nwait;wait;wait\n-use\n{}", secs(0.5));
    cfg += &format!("+attack\nwait;wait;wait\n-attack\n{}", secs(0.5));
    // Minor 2 (#306): a finished duck sets `local::DUCKING`.
    cfg += &format!("+duck\n{}-duck\n{}", secs(2.0), secs(0.5));
    cfg += &format!("ts_bot_stop 1\nts_bot_quota 1\n{}", secs(8.0));
    cfg += &format!("ts_tp 2 0 -1200 60 270 0\n{}ts_tp 1 0 -1344 60 90 0\n{}ts_face 1 2\n{}", secs(0.5), secs(1.0), secs(0.5));
    // Minor 1: `TSExt` both ways (with the bot in, for the server's "not a port client"), the
    // command hooks on the port's `special` and its `_special`, the chat hooks.
    // This build reports the last release's version, below `TSPKG_SINCE`: `ts_dev_tspkg 1` lets
    // `TSExt` reach it.
    cfg += &format!("ts_dev_tspkg 1\nts_dev_extlog 1\necho XTASK: tsext\ntsext_test_tsext\n{}", secs(1.0));
    for arm in ["block", "pass", "rename"] {
        cfg += &format!("tsext_test_arm {arm}\nspecial\n{}", secs(0.5));
    }
    cfg += "tsext_test_arm none\n";
    cfg += &format!("say tsext_block_me\n{}say tsext_hold_me\n{}say tsext_pass_me\n{}", secs(1.0), secs(2.0), secs(1.0));
    // Minor 2 (#314): the health box and chat hidden and shown again; the server's MOTD (its
    // answer to `motd`) taken over, answered, given back on `menu_shown`, and taken over again
    // for the suspension below to give back.
    cfg += &format!("motdfile {MOTD_FILE}\n");
    cfg += &format!("echo XTASK: hud\ntsext_test_hud hide\n{}tsext_test_hud show\n{}", secs(1.0), secs(0.5));
    cfg +=
        &format!("echo XTASK: menus\ntsext_test_menu take\nmotd\n{}tsext_test_menu bad\ntsext_test_menu close\n{}", secs(1.0), secs(0.5));
    cfg += &format!("motd\n{}tsext_test_menu giveback\n{}motd\n{}", secs(1.0), secs(1.0), secs(1.0));
    cfg += "motdfile xtask_no_motd.txt\n";
    cfg += &format!("echo XTASK: spawn\ntsext_test_spawn\n{}", secs(1.0));
    cfg += &format!("echo XTASK: suspend\nts_dev_extsuspend test 1\n{}tsext_test_report\n{}", secs(1.0), secs(0.5));
    cfg += &format!("ts_dev_extsuspend test 0\n{}", secs(0.5));
    cfg += &format!("echo XTASK: report\ntsext_test_report\n{}", secs(0.5));
    // Minor 2 (#306): the bot in view dies; it must leave `visible_players` while it does.
    // (`ts_hurt`, not `ts_as 2 kill`: `kill` is the engine's `ClientKill`, not a client command.)
    cfg += &format!("ts_hurt 2 500\n{}", secs(1.0));
    cfg += &format!("echo XTASK: kill\nkill\n{}", secs(2.0));
    cfg += "tsext_test_report\nts_dev_extlist\ncl_plugins_list\ncl_plugins_verify\nts_bot_quota 0\n";
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
        ("the development override announced".into(), text.contains("TSRS_DEV_CLIENT_EXT: approving test_refuse")),
        ("tsmod_test loaded from the store".into(), line(format!("loaded tsmod_test{sfx}"), "manual:test, approved by manual")),
        ("another API version skipped".into(), line(format!("tsmod_test_badver{sfx}"), "built for client extension API version")),
        ("a bad descriptor size skipped".into(), line(format!("tsmod_test_badsize{sfx}"), "descriptor")),
        ("a refusal skipped".into(), line(format!("tsmod_test_refuse{sfx}"), "refused")),
        ("a library without the export skipped".into(), line(format!("tsmod_noexport{sfx}"), "TSRS_ClientExtLoad export")),
        ("an unapproved library not loaded".into(), line(format!("tsmod_unapproved{sfx}"), "not approved (new plugin)")),
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
        // Minor 2 (#314).
        ("the HUD hidden".into(), ext("hud hidden by test: 0x41", "")),
        ("the MOTD taken over".into(), ext("MOTD taken over by test", "")),
        ("the taken-over MOTD answered".into(), ext("menu 3 answered: Close", "")),
        (
            "taken-over menus failed open (menu_shown, suspension)".into(),
            text.lines().filter(|l| l.contains("[ts ext] menu 3 of test given back to the port")).count() >= 2,
        ),
        // Issue #315: the clash check behind add_command/register_cvar is quiet on FWGS.
        ("no engine warning for a new command or cvar".into(), !text.contains("non-existent cvar \"tsext_")),
        ("no panic or crash".into(), !(text.contains("PANIC") || text.contains("FATAL ERROR") || text.contains("Segmentation"))),
    ];
    for what in tsmod_client_test::CHECKS {
        let ok = report.lines().any(|l| l.trim_end() == format!("check {what} ok"));
        c.push((format!("check {what}"), ok));
    }
    c
}

/// The prompt-log record of plugin `id`'s box, if it was shown.
fn prompt_of<'a>(prompts: &'a str, id: &str) -> Option<&'a str> {
    let head = format!("--- prompt {id}\n");
    let start = prompts.find(&head)? + head.len();
    let rest = &prompts[start..];
    Some(&rest[..rest.find("--- prompt ").unwrap_or(rest.len())])
}

/// The approval checks (#297, client-plugins.md §11 e2e 1): what loaded, what was asked and what
/// the answers wrote.
pub fn approval_checks(text: &str, t: &Target, prompts: &str, store: &Store, pkg_spawned: &str) -> Vec<(String, bool)> {
    let sfx = suffix(t);
    let ext = |file: &str, what: &str| {
        text.lines().any(|l| l.contains("[ts] client extensions:") && l.contains(&format!("tsmod_{file}{sfx}")) && l.contains(what))
    };
    // Asked about `id` saying `what`, with or without an "Allow" button.
    let asked = |id: &str, allow: bool, what: &str| {
        prompt_of(prompts, id).is_some_and(|p| p.contains("buttons: Allow,") == allow && p.contains("Never\n") && p.contains(what))
    };
    let state = |id: &str| store.entries_for(id).map(|e| e.state).next();
    let tampered = |id: &str, change: &str| {
        (format!("{id}: not loaded, named \"{change}\", asked with no Allow"), {
            ext(id, &format!("can't be approved as installed: {change}")) && asked(id, false, change) && !ext(id, "loaded ")
        })
    };
    vec![
        ("the prompt hook announced".into(), text.contains("TSRS_DEV_PLUGIN_PROMPT: answering the start-up prompts")),
        ("the plugins were hashed".into(), text.lines().any(|l| l.contains("[ts] client plugins: verified ") && l.contains("plugin(s)"))),
        (
            "an approved package loaded".into(),
            text.lines().any(|l| l.contains(&format!("loaded tsmod_quietpkg{sfx}")) && l.contains("quietpkg, approved by manual")),
        ),
        ("its init ran".into(), text.contains("[tsmod_quietpkg] quiet stub init")),
        (
            "its helper started through verified_spawn, an outside file refused".into(),
            text.contains("[tsmod_quietpkg] quiet spawn refused true started true") && pkg_spawned.contains("helper ran: from-quiet"),
        ),
        tampered("quiettamper_byte", "changed: files/data/readme.txt"),
        tampered("quiettamper_extra", "extra file: files/data/extra.txt"),
        tampered("quiettamper_missing", "missing: files/data/readme.txt"),
        (
            "an unmet [requires] port skipped without a box".into(),
            ext("quietport", "needs port <0.1") && prompt_of(prompts, "quietport").is_none(),
        ),
        ("a server-scoped approval waited at start".into(), ext("quietserver", "approved for loopback only")),
        (
            "and loaded on connecting to that server".into(),
            text.contains(&format!("loaded tsmod_quietserver{sfx}")) && text.contains("[tsmod_quietserver] quiet stub init"),
        ),
        ("a new library asked about".into(), asked("manual:quiet_allow", true, "new plugin")),
        ("the box names the risk".into(), asked("manual:quiet_allow", true, "runs inside the game with your account's permissions")),
        (
            "\"Allow\" stored and loaded it".into(),
            state("manual:quiet_allow") == Some(State::Allowed)
                && ext("quiet_allow", "loaded ")
                && text.contains("[tsmod_quiet_allow] quiet stub init"),
        ),
        ("\"Never\" stored a block".into(), state("manual:quiet_never") == Some(State::Blocked) && ext("quiet_never", "blocked")),
        ("\"Not now\" stored nothing".into(), state("manual:unapproved").is_none() && prompt_of(prompts, "manual:unapproved").is_some()),
        (
            "nothing approved or unloadable was asked about".into(),
            ["manual:test", "quietpkg", "manual:junk", "manual:wrongarch", "manual:test_refuse", "quietserver"]
                .iter()
                .all(|id| prompt_of(prompts, id).is_none()),
        ),
        (
            "cl_plugins_list".into(),
            text.lines().any(|l| l.contains("[ts] client plugins:   quietpkg 0.1.0: allowed (unsigned, from manual); loaded")),
        ),
        ("cl_plugins_verify".into(), text.lines().any(|l| l.contains("[ts] client plugins:   quiettamper_extra 0.1.0: can't be approved"))),
    ]
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
        for f in [
            "tsmod_test_amd64.so",
            "tsmod_test.files",
            "tsmod_test_helper",
            "tsmod_junk.dll",
            "tsmod_quiet_allow_amd64.so",
            "tsmod_webview_amd64.so",
            "client_amd64.so",
        ] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        remove_fixtures(&dir);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(left, ["client_amd64.so", "tsmod_webview_amd64.so"], "only xtask's own files go");
    }

    #[test]
    fn the_root_store_comes_back() {
        let game = std::env::temp_dir().join(format!("xtask-cl-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&game);
        fs::create_dir_all(game.join("cl_plugins/webview")).unwrap();
        fs::create_dir_all(game.join("cl_plugins/quietpkg/0.1.0")).unwrap();
        fs::write(store_path(&game), "mine").unwrap();
        set_store_aside(&game);
        assert!(!store_path(&game).exists());
        fs::write(store_path(&game), "xtask's").unwrap();
        remove_plugin_fixtures(&game);
        assert_eq!(fs::read_to_string(store_path(&game)).unwrap(), "mine");
        assert!(game.join("cl_plugins/webview").exists() && !game.join("cl_plugins/quietpkg").exists());
        // No store before: none after.
        fs::remove_file(store_path(&game)).unwrap();
        set_store_aside(&game);
        fs::write(store_path(&game), "xtask's").unwrap();
        remove_plugin_fixtures(&game);
        let gone = !store_path(&game).exists();
        let _ = fs::remove_dir_all(&game);
        assert!(gone);
    }

    #[test]
    fn a_passing_run() {
        let t = Target::LINUX_X86_64;
        let mut log =
            String::from("[ts] client extensions: TSRS_DEV_CLIENT_EXT: approving test_refuse without hashing (debug build only)\n");
        for l in [
            "loaded tsmod_test_amd64.so (tsmod_test (client stub) 1; manual:test, approved by manual)",
            "tsmod_test_badver_amd64.so: built for client extension API version 2, this client has version 1, skipped",
            "tsmod_test_badsize_amd64.so: its extension descriptor is 8 bytes, this client's is 200, skipped",
            "tsmod_test_refuse_amd64.so: its TSRS_ClientExtLoad refused to load (-2), skipped",
            "tsmod_noexport_amd64.so: has no TSRS_ClientExtLoad export, skipped",
            "tsmod_unapproved_amd64.so: not approved (new plugin); asks again at the next start, skipped",
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
            "hud hidden by test: 0x41",
            "MOTD taken over by test",
            "menu 3 answered: Close",
            "menu 3 of test given back to the port",
            "menu 3 of test given back to the port",
        ] {
            log += &format!("[ts ext] {l}\n");
        }
        log +=
            "[tsmod_test] tsext_echo to #1: echo:hello bot=-3 all=1 big=-2 badch=-2 ver=3.0.8 (1)\nHost killed self with tsext_rewritten\n";
        log += "[tsmod_test] warning: its replacement for server command _special changes the command's name: blocked\n";
        let warned = client_plugin_checks(
            &(log.clone() + "Warning: CL_CvarGetPointer: client tried to get non-existent cvar \"tsext_test_cvar\"\n"),
            &t,
            "",
            "",
        );
        assert!(warned.iter().any(|(w, ok)| w.starts_with("no engine warning") && !ok));
        let report: String = tsmod_client_test::CHECKS.iter().map(|c| format!("check {c} ok\n")).collect();
        let checks = client_plugin_checks(&log, &t, &report, "helper ran: from-tsmod_test\n");
        assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
        let bad = client_plugin_checks(&log, &t, &report.replace("check aim ok", "check aim FAIL x"), "");
        assert!(bad.iter().any(|(w, ok)| w == "check aim" && !ok));
        assert!(bad.iter().any(|(w, ok)| w == "the helper process ran" && !ok));
    }

    #[test]
    fn prompt_records() {
        let log = "--- prompt a\nbuttons: Allow, Not now, Never\nx\nanswer: allow\n--- prompt b\nbuttons: Not now, Never\ny\n";
        assert_eq!(prompt_of(log, "a"), Some("buttons: Allow, Not now, Never\nx\nanswer: allow\n"));
        assert_eq!(prompt_of(log, "b"), Some("buttons: Not now, Never\ny\n"));
        assert_eq!(prompt_of(log, "c"), None);
        let none = approval_checks("", &Target::LINUX_X86_64, "", &Store::new(), "");
        assert!(none.iter().all(|(w, ok)| !ok || w.starts_with("\"Not now\"") || w.starts_with("nothing")), "{none:?}");
    }
}
