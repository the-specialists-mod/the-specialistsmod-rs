//! `cargo xtask plugins [roots...]`: the `tsmod_*` loader and the plugin bot backend on dedicated
//! servers (issue #104, docs/implementation/bot-plugins.md §6). On each root it installs the stub
//! plugin `tsmod_test` next to the server library, plus fixtures the loader must skip with a
//! warning (another API version, a bad descriptor size, a refusal, no export, the other
//! architecture, junk, another platform's name). The server starts on ts_dojo with native bots,
//! switches `ts_bot_backend` to `test` and changes the map, and the test backend's bots must
//! walk, jump and answer the plugin's API checks (with version 5's: a listener, a dive, a used
//! power-up, which needs `sv_cheats`, kung-fu moves changed and blocked, a scored kill; and
//! version 6's: the per-frame players, items and frame, `run_cmd`'s view, `msec` and events, the
//! touch, damage, shot and pickup callbacks, `entity_key`; the shots need `sv_cheats` too, for
//! `ts_give`). The
//! fixtures are removed afterwards (and by every `install`), so other scenarios run without
//! plugins.

use crate::roots::{Os, Root, Target};
use crate::{Ctx, say};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::time::{Duration, Instant};

/// The `tsmod_*` library suffix of a build target: the server library's naming
/// (`ts_plugin_api::library_suffix`).
pub fn suffix(t: &Target) -> &'static str {
    ts_plugin_api::library_suffix(t.os == Os::Windows, t.triple.starts_with("x86_64"))
}

/// The same OS's other architecture (the wrong-architecture fixture).
fn other_arch(t: &Target) -> Target {
    *Target::ALL.iter().find(|o| o.os == t.os && o.triple != t.triple).expect("two architectures per OS")
}

/// The fixtures' names (`<name>` of `tsmod_<name>`), besides `test` itself.
const FIXTURES: [&str; 7] = ["test_badver", "test_badsize", "test_refuse", "noexport", "wrongarch", "junk", "otherplat"];

/// The plugin checks the stub logs as `[tsmod_test] check <what> ok`. From `add_listener` on:
/// plugin API version 5 (#198); from `players_count` on, version 6; from `attach_plugin` on,
/// version 7 (#290); from `ext_message` on, version 8 (#294, #304, #321).
pub const CHECKS: [&str; 56] = [
    "data_dir",
    "map_name",
    "create_bot",
    "run_cmd",
    "player_info",
    "entity_info",
    "game_state",
    "team_scores",
    "objectives",
    "create_entity",
    "gun_hook",
    "inventory",
    "bot_command",
    "client_command",
    "spawned",
    "buy",
    "buy_item",
    "moved",
    "sound",
    "add_listener",
    "listener",
    "remove_listener",
    "private_size",
    "player_stunt",
    "player_powerup",
    "melee_attack",
    "melee_attack_change",
    "melee_attack_block",
    "ham_take_damage",
    "kill_scored",
    "players_count",
    "players",
    "frame_info",
    "items",
    "inventory_serial",
    "combat_power",
    "combat",
    "run_cmd_time",
    "run_cmd_result",
    "cmd_fixangle",
    "set_view",
    "cmd_landed",
    "cmd_teleported",
    "entity_key",
    "sound_source",
    "entity_touch",
    "damage",
    "weapon_fired",
    "item_pickup",
    "attach_plugin",
    "detach_plugin",
    "snapshot_powers",
    "ext_message",
    "client_port_version",
    "offer_client_plugin",
    "client_plugin_version",
];

/// Removes every `tsmod_*` file xtask may have put in `dlls/` (a run that was cut short).
pub fn remove_fixtures(dlls: &Path) {
    let Ok(rd) = fs::read_dir(dlls) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let ours = std::iter::once("test").chain(FIXTURES).any(|f| name.starts_with(&format!("tsmod_{f}")));
        if ours {
            let _ = fs::remove_file(e.path());
        }
    }
}

impl Ctx {
    /// `libtsmod_test.so` / `tsmod_test.dll` for `t`.
    pub fn test_plugin_artifact(&self, t: &Target) -> PathBuf {
        let dir = self.repo.join("target").join(t.triple).join(self.profile());
        dir.join(if t.os == Os::Windows { "tsmod_test.dll" } else { "libtsmod_test.so" })
    }

    pub fn plugins_all(&self, roots: &[Root]) -> bool {
        // The wrong-architecture fixture needs the other architecture's build too.
        for t in Target::ALL {
            let mut c = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
            c.current_dir(&self.repo).args(["build", "-p", "tsmod-test", "--target", t.triple]);
            if self.profile != crate::Profile::Dev {
                c.arg("--release");
            }
            if !c.status().expect("cargo").success() {
                exit(1);
            }
        }
        self.parallel(roots, |r, i| self.plugins(r, 27515 + i as u16))
    }

    /// Puts the stub and the fixtures into `<root>/ts/dlls`.
    fn install_fixtures(&self, t: &Target, dlls: &Path) {
        let sfx = suffix(t);
        let plugin = self.test_plugin_artifact(t);
        for name in ["test", "test_badver", "test_badsize", "test_refuse"] {
            crate::copy(&plugin, &dlls.join(format!("tsmod_{name}{sfx}")));
        }
        // A real library without the export: a copy of the server library itself.
        crate::copy(&dlls.join(t.server_name), &dlls.join(format!("tsmod_noexport{sfx}")));
        crate::copy(&self.test_plugin_artifact(&other_arch(t)), &dlls.join(format!("tsmod_wrongarch{sfx}")));
        fs::write(dlls.join(format!("tsmod_junk{sfx}")), b"not a library").unwrap();
        let other_os = Target::ALL.iter().find(|o| o.os != t.os).expect("two platforms");
        fs::write(dlls.join(format!("tsmod_otherplat{}", suffix(other_os))), b"skipped by name").unwrap();
    }

    fn plugins(&self, root: &Root, port: u16) -> bool {
        let t = root.target();
        let gamedir = root.path.join("ts");
        let dlls = gamedir.join("dlls");
        self.install_fixtures(&t, &dlls);
        let map = "ts_dojo";
        say!("== {}: tsmod_* plugins and the test bot backend, {map}, port {port}", root.name);
        // The first map runs native; its server config selects the test backend, which reloads
        // the map (`ts_bot_backend` is read at map start; no `changelevel`), and sets
        // `ts_gameplay` to 2.1 and back, which must not. The reloaded map's config (and every
        // later map's) adds bots and names the running backend in capitals: no second reload.
        // `sv_cheats 1` before the reload (TS latches it at the map's start) for the stub's
        // `powerup` cheat.
        fs::write(
            gamedir.join("xtask_plugins.cfg"),
            format!(
                "// Written by cargo xtask plugins.\n{}sv_cheats 1\nts_bot_quota 2\nts_bot_backend test\nts_gameplay 21\nts_gameplay 30\nservercfgfile xtask_plugins2.cfg\n",
                crate::roots::EXEC_SERVER_CFG
            ),
        )
        .unwrap();
        fs::write(
            gamedir.join("xtask_plugins2.cfg"),
            "// Written by cargo xtask plugins.\nts_bot_quota 2\nts_bot_backend TEST\naddbot\naddcustombot \"Custom Test\" \"\" 0.5\n",
        )
        .unwrap();
        let log = root.log_path();
        let _ = fs::remove_file(&log);
        let mut cmd = root.server_command_with(map, port, "xtask_plugins.cfg", 2);
        let out = fs::File::create(self.repo.join("target").join(format!("server-{}.log", root.name))).unwrap();
        cmd.stdout(out.try_clone().unwrap()).stderr(out);
        let mut child = cmd.spawn().expect("start server");
        let start = Instant::now();
        let limit = Duration::from_secs(if root.os == Os::Windows { 120 } else { 60 });
        let mut up = false;
        while start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(500));
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            if crate::a2s::info("127.0.0.1", port).is_ok() {
                up = true;
                break;
            }
        }
        if up {
            // The map change, the quota's second, and time for the bots to walk and respawn.
            std::thread::sleep(Duration::from_secs(30));
        }
        let _ = child.kill();
        let _ = child.wait();
        root.cleanup();
        remove_fixtures(&dlls);
        let text = root.read_log(&self.repo);
        for line in text.lines().filter(|l| l.contains("[ts] plugins") || l.contains("[ts] bots: backend") || l.contains("[tsmod_test]")) {
            say!("   | {}", line.trim_end());
        }
        let checks = plugin_checks(&text, &t, up);
        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        let ok = checks.iter().all(|(_, ok)| *ok);
        say!("   {}", if ok { "PASS" } else { "FAIL" });
        ok
    }
}

/// The server config's `ts_bot_backend test` (and `ts_gameplay` 21 and back): the Steam engines run
/// the config once the first map has loaded, so it reloads that map once, for the backend; FWGS's
/// dedicated server also runs it before the first map, which then starts with the test backend and
/// needs no reload. Never a reload for the ruleset, never a second one.
fn server_config_reload_ok(text: &str) -> bool {
    let reloads = text.matches(": reloading ").count();
    let backend = text.matches("[ts] bots: backend test: reloading ts_dojo").count();
    let first_backend = text.lines().find_map(|l| l.split_once("[ts] bots: backend ").map(|(_, b)| b.trim_end().to_owned()));
    let no_ruleset = !text.contains("[ts] gameplay: ruleset 21");
    no_ruleset && (reloads == 1 && backend == 1 || reloads == 0 && first_backend.as_deref() == Some("test"))
}

/// The checks on a `plugins` run's log.
pub fn plugin_checks(text: &str, t: &Target, up: bool) -> Vec<(String, bool)> {
    let sfx = suffix(t);
    let other_os = Target::ALL.iter().find(|o| o.os != t.os).expect("two platforms");
    let line = |file: String, what: &str| text.lines().any(|l| l.contains("[ts] plugins:") && l.contains(&file) && l.contains(what));
    let mut c: Vec<(String, bool)> = vec![
        ("server answered".into(), up),
        ("the Rust server library loaded".into(), text.contains("[ts] GameInit")),
        ("tsmod_test loaded".into(), line(format!("loaded tsmod_test{sfx}"), "")),
        ("another API version skipped".into(), line(format!("tsmod_test_badver{sfx}"), "built for plugin API version")),
        ("a bad descriptor size skipped".into(), line(format!("tsmod_test_badsize{sfx}"), "descriptor")),
        ("a refusal skipped".into(), line(format!("tsmod_test_refuse{sfx}"), "refused")),
        ("a library without the export skipped".into(), line(format!("tsmod_noexport{sfx}"), "TSRS_PluginLoad export")),
        ("the other architecture skipped".into(), line(format!("tsmod_wrongarch{sfx}"), "built for")),
        ("junk skipped".into(), line(format!("tsmod_junk{sfx}"), "not a library")),
        ("another platform's name skipped".into(), line(format!("tsmod_otherplat{}", suffix(other_os)), "library (this server is")),
        ("the test backend selected".into(), text.lines().any(|l| l.trim_end().ends_with("[ts] bots: backend test"))),
        (
            "ts_bot_backend test in the server config: at most one reload of ts_dojo, none for ts_gameplay 21 and back".into(),
            server_config_reload_ok(text),
        ),
        (
            "no plugin check failed".into(),
            !text.contains("check") || !text.lines().any(|l| l.contains("[tsmod_test]") && l.contains("FAIL")),
        ),
        // Issue #331: the clash check before registering the server's and plugins' cvars is quiet on FWGS.
        ("no engine warning for a new cvar".into(), !text.contains("server tried to get non-existent cvar")),
        ("no panic or crash".into(), !(text.contains("PANIC") || text.contains("FATAL ERROR") || text.contains("Segmentation"))),
    ];
    for what in CHECKS {
        c.push((format!("check {what}"), text.contains(&format!("[tsmod_test] check {what} ok"))));
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_and_fixtures() {
        assert_eq!(suffix(&Target::WINDOWS_I686), ".dll");
        assert_eq!(suffix(&Target::WINDOWS_X86_64), "_amd64.dll");
        assert_eq!(suffix(&Target::LINUX_I686), "_i386.so");
        assert_eq!(suffix(&Target::LINUX_X86_64), "_amd64.so");
        assert_eq!(other_arch(&Target::LINUX_I686).triple, Target::LINUX_X86_64.triple);
        assert_eq!(other_arch(&Target::WINDOWS_X86_64).triple, Target::WINDOWS_I686.triple);
        let dir = std::env::temp_dir().join(format!("xtask-fixtures-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        for f in ["tsmod_test_i386.so", "tsmod_junk.dll", "tsmod_rcbot_i386.so", "ts_i386.so"] {
            fs::write(dir.join(f), b"x").unwrap();
        }
        remove_fixtures(&dir);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(left, ["ts_i386.so", "tsmod_rcbot_i386.so"], "only xtask's own files go");
    }

    #[test]
    fn a_passing_log() {
        let t = Target::LINUX_X86_64;
        let mut log = String::from("[ts] GameInit: x\n[ts] bots: backend test: reloading ts_dojo\n[ts] bots: backend test\n");
        for l in [
            "loaded tsmod_test_amd64.so (tsmod_test 1)",
            "tsmod_test_badver_amd64.so: built for plugin API version 8, this server has version 7, skipped",
            "tsmod_test_badsize_amd64.so: its plugin descriptor is 8 bytes, skipped",
            "tsmod_test_refuse_amd64.so: its TSRS_PluginLoad refused to load (-2), skipped",
            "tsmod_noexport_amd64.so: has no TSRS_PluginLoad export, skipped",
            "tsmod_wrongarch_amd64.so: built for linux-i386 (this server is linux-amd64), skipped",
            "tsmod_junk_amd64.so: not a library for this server (linux-amd64), skipped",
            "tsmod_otherplat.dll: a win32 library (this server is linux-amd64), skipped",
        ] {
            log += &format!("[ts] plugins: {l}\n");
        }
        for c in CHECKS {
            log += &format!("[tsmod_test] check {c} ok\n");
        }
        let checks = plugin_checks(&log, &t, true);
        assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
        let bad = plugin_checks(&format!("{log}[tsmod_test] error: check moved FAIL\n"), &t, true);
        assert!(bad.iter().any(|(w, ok)| w == "no plugin check failed" && !ok));
        let warned =
            plugin_checks(&format!("{log}Warning: SV_CvarGetPointer: server tried to get non-existent cvar \"ts_bot_quota\"\n"), &t, true);
        assert!(warned.iter().any(|(w, ok)| w == "no engine warning for a new cvar" && !ok));
        let looped = plugin_checks(&format!("{log}[ts] bots: backend test: reloading ts_dojo\n"), &t, true);
        assert!(looped.iter().any(|(w, ok)| w.contains("one reload") && !ok), "a second reload fails");
    }

    /// The server config's reload, as the Steam engines and FWGS log it.
    #[test]
    fn server_config_reload() {
        let steam = "[ts] bots: backend native\n[ts] bots: backend test: reloading ts_dojo\n[ts] bots: backend test\n";
        let fwgs = "[01:40:37] [ts] bots: backend test\r\n";
        assert!(server_config_reload_ok(steam));
        assert!(server_config_reload_ok(fwgs), "the config ran before the first map");
        assert!(!server_config_reload_ok("[ts] bots: backend native\n[ts] bots: backend test\n"), "a changelevel, not a reload");
        assert!(!server_config_reload_ok(&format!("{steam}[ts] bots: backend test: reloading ts_dojo\n")), "twice");
        assert!(!server_config_reload_ok(&format!("{fwgs}[ts] gameplay: ruleset 21: reloading ts_dojo\n")), "the ruleset's");
    }
}
