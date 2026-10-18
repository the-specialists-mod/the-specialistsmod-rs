//! `cargo xtask relaunch [--xvfb] [fwgs] [fwgs-win]`: restart and reconnect end to end (issue
//! #301, docs/implementation/client-plugins.md §11 e2e 5 and 6, §13.5) with the port's FWGS
//! client (Linux, and win32 under Wine), against dedicated servers on the `fwgs` root: one open,
//! one with `sv_password`. The client is built with `--features e2e` (cards answered from a
//! file, the self-relaunch forced on by `TSRS_E2E_SELF_RELAUNCH`), and `ts-launcher-fake`
//! (crates/fake-launcher) stands in for tsmod-launcher. Each scenario starts a client that joins,
//! runs `ts_dev_restart`, and passes when the client's process is gone and the restarted game is
//! on the server (A2S shows the player again):
//!
//! 1. **IPC**: the fake launcher serves `TSMOD_LAUNCHER_IPC`; the client sends `relaunch` and
//!    quits; the fake launcher restarts it with `+connect`.
//! 2. **IPC without an answer** (`answer=silent`): after 2 s the client starts `TSMOD_LAUNCHER`
//!    as `ts-launcher-fake relaunch ...` instead; passworded server, so the arguments carry
//!    `--password-required` and the restarted client shows the password card (answered with the
//!    password through the e2e hook) before it connects.
//! 3. **`launcher.toml` only**, passworded: the same through the arguments alone.
//! 4. **Self-relaunch**, no launcher: the engine re-executes itself with `+connect`.
//! 5. **Quit and inform**, passworded, no launcher, self-relaunch off: the card is shown, "Quit
//!    now" writes `.reconnect` (address, `password_required = true`), the game quits, nobody
//!    rejoins.
//! 6. **The next start**: with `.reconnect` from 5, a plain start shows "Reconnect to ..." with
//!    the password field; cancelled, nothing connects; with a fresh `.reconnect` and the card
//!    answered (password typed), the game joins.
//!
//! In every scenario the password must not appear in the fake launcher's log (each IPC line,
//! each argument and the whole environment it got), nor in `.reconnect`. Scenario 4 passing is
//! what lets a platform into `SELF_RELAUNCH_VERIFIED` (crates/client/src/relaunch/plan.rs).
//!
//! "Joined" means in the game: rcon `status` lists an `xtask_e2e` that is spawned (a ping, not
//! `Connect`/`Spawning`/`Zombie`) and sent something in the last 3 s. The A2S player count isn't
//! enough: it counts a client from its first `connect` packet, and a game the previous scenario
//! killed (no `disconnect`) stays on the list until `sv_timeout` drops it. So every scenario also
//! starts on an empty server (A2S shows no players). The servers run `sv_timeout 20` and
//! `sv_connect_timeout 30` so that wait is short, and `sv_connect_timeout_ban 0`: FWGS otherwise
//! bans the address of a client that timed out while connecting (`addip` for 2 min), and killing
//! a game mid-connect would lock 127.0.0.1 out of the next scenarios.
//!
//! `--xvfb` runs the games on a private X server (`~/scratch/xvfb/usr/bin/Xvfb`, or `XVFB`).
//! `--only <name>[,<name>...]` runs some scenarios (`ipc`, `ipc-silent`, `relaunch-args`,
//! `self`, `inform`, the last with the next-start ones). Under Wine the engine's `engine.log` is
//! kept per scenario as `target/relaunch/<root>-<scenario>-engine.log`. About 5 min per root.
//! Not in `all`.

use crate::a2s;
use crate::roots::{self, EXEC_SERVER_CFG, Os, Root, Target};
use crate::{Ctx, Profile, say};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PASSWORD: &str = "e2e-secret-4711";
const RCON: &str = "xtask-relaunch-rcon";
const CLIENT_CFG: &str = "xtask_relaunch.cfg";
pub(crate) const LAUNCHER_DIR: &str = "xtask_launcher";

fn server_cfg(password: bool) -> String {
    let pw = if password { format!("sv_password \"{PASSWORD}\"\n") } else { "sv_password \"\"\n".into() };
    format!(
        "// Written by cargo xtask relaunch.\n{EXEC_SERVER_CFG}developer 1\nmp_timelimit 0\nmp_fraglimit 0\nts_bot_quota 0\nsv_timeout 20\nsv_connect_timeout 30\nsv_connect_timeout_ban 0\nsv_max_queries_sec 1000\nsv_max_queries_sec_global 10000\nrcon_password \"{RCON}\"\n{pw}"
    )
}

/// A path as the root's programs see it: Windows programs under Wine get the `Z:` drive's name.
fn native(root: &Root, p: &Path) -> String {
    if root.os == Os::Windows { format!("Z:{}", p.display().to_string().replace('/', "\\")) } else { p.display().to_string() }
}

struct Scenario {
    name: &'static str,
    password: bool,
    ipc: Option<&'static str>,
    launcher_toml: bool,
    self_relaunch: bool,
    /// The e2e answers (`<kind>=ok|cancel`, `typed=`).
    answers: String,
}

struct Servers {
    open: u16,
    passworded: u16,
    children: Vec<Child>,
}

impl Ctx {
    pub fn relaunch_all(&self, args: &[String]) -> bool {
        let mut names = args.to_vec();
        let xvfb = crate::take_flag(&mut names, "--xvfb");
        let only: Option<Vec<String>> = crate::take_opt(&mut names, "--only").map(|o| o.split(',').map(str::to_owned).collect());
        let server = Root::all().into_iter().find(|r| r.name == "fwgs").unwrap();
        if !server.exists() {
            say!("== relaunch: skipped (no fwgs root for the servers)");
            return true;
        }
        let clients = crate::play_roots(&names);
        let display = if xvfb { start_xvfb() } else { None };
        if xvfb && display.is_none() {
            say!("== relaunch: FAIL: --xvfb, but Xvfb didn't start");
            return false;
        }
        let mut ok = true;
        for (i, r) in clients.iter().filter(|r| r.exists()).enumerate() {
            if r.name != server.name {
                self.install(&server);
            }
            self.install_e2e(r);
            let base = 27915 + 20 * i as u16;
            ok &= self.relaunch_root(&server, r, base, display.as_ref().map(|d| d.1.as_str()), only.as_deref());
            // Put the normal client back.
            self.install(r);
        }
        if let Some((mut x, _)) = display {
            let _ = x.kill();
            let _ = x.wait();
        }
        say!("== relaunch: {}", crate::pass(ok));
        ok
    }

    /// Installs `root` with the e2e client (the `e2e` feature) and copies `ts-launcher-fake` to
    /// `<root>/xtask_launcher/`.
    pub(crate) fn install_e2e(&self, root: &Root) {
        self.install(root);
        let t = root.target();
        let mut c = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
        c.current_dir(&self.repo).args([
            "build",
            "-p",
            "ts-client-dll",
            "--features",
            "ts-client-dll/e2e",
            "-p",
            "tsmod-fake-launcher",
            "--target",
            t.triple,
        ]);
        if self.profile != Profile::Dev {
            c.arg("--release");
        }
        say!("== cargo build --features e2e --target {}", t.triple);
        if !c.status().expect("cargo").success() {
            std::process::exit(1);
        }
        let client = root.path.join("ts").join("cl_dlls").join(t.client_name);
        crate::copy(&self.artifact(&t, false), &client);
        if self.profile == Profile::Release {
            crate::strip(&t, &client);
        }
        if t.os == Os::Windows {
            crate::msvc_style_export_section(&client);
        }
        let exe = fake_name(&t);
        let dir = root.path.join(LAUNCHER_DIR);
        fs::create_dir_all(&dir).unwrap();
        crate::copy(&self.repo.join("target").join(t.triple).join(self.profile()).join(exe), &dir.join(exe));
        say!("== {}: e2e client and {exe} installed", root.name);
    }

    fn relaunch_root(&self, server: &Root, root: &Root, base: u16, display: Option<&str>, only: Option<&[String]>) -> bool {
        let wanted = |n: &str| only.is_none_or(|o| o.iter().any(|w| w == n));
        say!("== {}: restart and reconnect (servers on {}, ports {base} and {})", root.name, server.name, base + 1);
        let Some(mut servers) = self.relaunch_servers(server, base) else {
            say!("   FAIL: the servers didn't answer");
            return false;
        };
        // A game left over from an interrupted run would join the servers and pass every "joined".
        Run::new(self, root, "start", display).kill_games();
        let reconnect = root.path.join("ts").join("cl_plugins_data").join(".reconnect");
        let _ = fs::remove_file(&reconnect);
        let typed = format!("typed={PASSWORD}\n");
        let scenarios = [
            Scenario { name: "ipc", password: false, ipc: Some("ok"), launcher_toml: false, self_relaunch: false, answers: String::new() },
            Scenario {
                name: "ipc-silent",
                password: true,
                ipc: Some("silent"),
                launcher_toml: false,
                self_relaunch: false,
                answers: format!("password=ok\n{typed}"),
            },
            Scenario {
                name: "relaunch-args",
                password: true,
                ipc: None,
                launcher_toml: true,
                self_relaunch: false,
                answers: format!("password=ok\n{typed}"),
            },
            Scenario { name: "self", password: false, ipc: None, launcher_toml: false, self_relaunch: true, answers: String::new() },
        ];
        let mut checks: Vec<(String, bool)> = Vec::new();
        for s in scenarios.iter().filter(|s| wanted(s.name)) {
            let port = if s.password { servers.passworded } else { servers.open };
            checks.push(empty_server(s.name, port));
            checks.extend(self.restart_scenario(root, s, port, display));
        }
        if wanted("inform") {
            checks.push(empty_server("inform", servers.passworded));
            checks.extend(self.inform_scenarios(root, servers.passworded, &reconnect, display));
        }
        for c in &mut servers.children {
            let _ = c.kill();
            let _ = c.wait();
        }
        for p in [base, base + 1] {
            let _ = fs::remove_file(server.path.join("ts").join(format!("xtask_relaunch_server_{p}.cfg")));
        }
        server.cleanup();
        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        let ok = checks.iter().all(|(_, ok)| *ok);
        say!("   {}: {}", root.name, crate::pass(ok));
        ok
    }

    /// The open server on `base`, the passworded one on `base + 1`.
    fn relaunch_servers(&self, server: &Root, base: u16) -> Option<Servers> {
        let mut children = Vec::new();
        for (port, password) in [(base, false), (base + 1, true)] {
            let cfg = format!("xtask_relaunch_server_{port}.cfg");
            fs::write(server.path.join("ts").join(&cfg), server_cfg(password)).unwrap();
            let mut cmd = server.server_command_with("ts_dojo", port, &cfg, 0);
            let out = fs::File::create(self.repo.join("target").join(format!("relaunch-server-{port}.log"))).unwrap();
            cmd.stdout(out.try_clone().unwrap()).stderr(out);
            children.push(cmd.spawn().expect("start server"));
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(500));
            if a2s::info("127.0.0.1", base).is_ok() && a2s::info("127.0.0.1", base + 1).is_ok() {
                return Some(Servers { open: base, passworded: base + 1, children });
            }
        }
        for mut c in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        None
    }

    /// Scenarios 1 to 4: a client joins, restarts, and the restarted game must be back.
    fn restart_scenario(&self, root: &Root, s: &Scenario, port: u16, display: Option<&str>) -> Vec<(String, bool)> {
        let n = s.name;
        say!("   -- {n}: port {port}{}", if s.password { " (passworded)" } else { "" });
        let run = Run::new(self, root, n, display);
        run.write_answers(&s.answers);
        let addr = format!("127.0.0.1:{port}");
        run.write_fake_cfg(s.ipc.unwrap_or("ok"));
        let toml = root.path.join("ts").join("launcher.toml");
        let _ = fs::remove_file(&toml);
        if s.launcher_toml {
            let exe = native(root, &run.fake_exe());
            fs::write(
                &toml,
                format!(
                    "format = 1\nlauncher = {exe:?}\nlauncher_version = \"0.0.0-fake\"\ninstance = \"e2e\"\nroot = {:?}\n",
                    native(root, &root.path)
                ),
            )
            .unwrap();
        }
        let mut env: Vec<(String, String)> = Vec::new();
        let mut serve = None;
        if let Some(answer) = s.ipc {
            let ep = run.endpoint();
            serve = Some(run.start_fake_serve(&ep));
            env.push(("TSMOD_LAUNCHER".into(), native(root, &run.fake_exe())));
            env.push(("TSMOD_LAUNCHER_INSTANCE".into(), "e2e".into()));
            env.push(("TSMOD_LAUNCHER_IPC".into(), ep));
            if !run.wait_log("listening on", Duration::from_secs(30)) {
                say!("   ({n}: the fake launcher didn't start; answer={answer})");
            }
        }
        if s.self_relaunch {
            env.push(("TSRS_E2E_SELF_RELAUNCH".into(), "1".into()));
        }
        let script = format!(
            "name xtask_e2e\n{}connect {addr}\n{}ts_dev_restart\n",
            if s.password { format!("password \"{PASSWORD}\"\n") } else { String::new() },
            waits(70)
        );
        let mut g1 = run.start_game(&script, &env);
        let g1_gone = wait_exit(&mut g1, Duration::from_secs(if root.os == Os::Windows { 150 } else { 90 }));
        let back = g1_gone && wait_in_game(port, Duration::from_secs(if root.os == Os::Windows { 120 } else { 60 }));
        // Stop everything this scenario started.
        let _ = g1.kill();
        let _ = g1.wait();
        run.kill_games();
        if let Some(mut f) = serve {
            let _ = f.kill();
            let _ = f.wait();
        }
        root.cleanup();
        run.keep_engine_log();
        let _ = fs::remove_file(&toml);
        let log = run.fake_log();
        let shown = run.shown();
        let mut c: Vec<(String, bool)> = vec![
            (format!("{n}: the first game exited after ts_dev_restart"), g1_gone),
            (format!("{n}: the restarted game joined {addr}"), back),
            (
                format!("{n}: the password is nowhere in the fake launcher's log (requests, arguments, environment)"),
                !log.contains(PASSWORD),
            ),
        ];
        match n {
            "ipc" => {
                c.push((
                    format!("{n}: the client sent relaunch over IPC"),
                    log.contains("ipc {\"op\":\"relaunch\"") && log.contains(&format!("\"connect\":\"{addr}\"")),
                ));
                c.push((
                    format!("{n}: the fake launcher waited for the pid and relaunched with +connect"),
                    log.contains(" exited after ") && log.contains(&format!("+connect {addr}")),
                ));
            }
            "ipc-silent" => {
                c.push((format!("{n}: the IPC request was sent, with password_required"), log.contains("\"password_required\":true")));
                c.push((
                    format!("{n}: then ts-launcher-fake relaunch was started"),
                    log.contains("arg relaunch") && log.contains("arg --password-required"),
                ));
            }
            "relaunch-args" => {
                c.push((format!("{n}: started as relaunch --instance e2e --wait-pid --connect {addr} --password-required"), {
                    [
                        "arg relaunch",
                        "arg --instance",
                        "arg e2e",
                        "arg --wait-pid",
                        "arg --connect",
                        &format!("arg {addr}"),
                        "arg --password-required",
                    ]
                    .iter()
                    .all(|w| log.contains(w))
                }));
                c.push((format!("{n}: no IPC (none running)"), !log.contains("ipc {")));
            }
            _ => {}
        }
        if s.password {
            c.push((
                format!("{n}: relaunched with +tsrs_reconnect {addr} password"),
                log.contains(&format!("+tsrs_reconnect {addr} password")),
            ));
            c.push((format!("{n}: the restarted game asked for the password in a card"), shown.contains("kind=password field=1")));
        }
        if n == "self" {
            c.push((format!("{n}: no launcher was involved"), log.is_empty()));
        }
        run.clean();
        c
    }

    /// Scenarios 5 and 6: quit and inform, then the next start's card.
    fn inform_scenarios(&self, root: &Root, port: u16, reconnect: &Path, display: Option<&str>) -> Vec<(String, bool)> {
        let addr = format!("127.0.0.1:{port}");
        let mut c = Vec::new();
        let limit = Duration::from_secs(if root.os == Os::Windows { 150 } else { 90 });

        say!("   -- inform: port {port} (passworded)");
        let run = Run::new(self, root, "inform", display);
        run.write_answers("inform=ok\n");
        let _ = fs::remove_file(reconnect);
        let script = format!("name xtask_e2e\npassword \"{PASSWORD}\"\nconnect {addr}\n{}ts_dev_restart\n", waits(70));
        // Self-relaunch off even once this platform is verified, so the card is reached.
        let mut g = run.start_game(&script, &[("TSRS_E2E_SELF_RELAUNCH".into(), "0".into())]);
        let gone = wait_exit(&mut g, limit);
        let rejoined = wait_players(port, |p| p > 0, Duration::from_secs(15));
        let _ = g.kill();
        let _ = g.wait();
        run.kill_games();
        root.cleanup();
        run.keep_engine_log();
        let text = fs::read_to_string(reconnect).unwrap_or_default();
        c.push(("inform: the card was shown".into(), run.shown().contains("kind=inform")));
        c.push(("inform: \"Quit now\" quit the game".into(), gone));
        c.push(("inform: nothing restarted it".into(), !rejoined));
        c.push((
            "inform: .reconnect names the server and password_required = true".into(),
            text.contains(&format!("address = \"{addr}\"")) && text.contains("password_required = true"),
        ));
        c.push(("inform: the password isn't in .reconnect".into(), !text.is_empty() && !text.contains(PASSWORD)));
        run.clean();

        // The next start must not find the inform game (it quit, but make sure) on the server.
        c.push(empty_server("next start", port));
        say!("   -- next start, card cancelled");
        let run = Run::new(self, root, "next-cancel", display);
        run.write_answers("reconnect=cancel\n");
        let mut g = run.start_game("name xtask_e2e\n", &[]);
        let joined = wait_players(port, |p| p > 0, Duration::from_secs(if root.os == Os::Windows { 60 } else { 30 }));
        let _ = g.kill();
        let _ = g.wait();
        run.kill_games();
        root.cleanup();
        run.keep_engine_log();
        let shown = run.shown();
        c.push((
            "next start: \"Reconnect to ...\" was offered, with the password field".into(),
            shown.contains("kind=reconnect field=1") && shown.contains(&addr),
        ));
        c.push(("next start: cancelled, nothing connected".into(), !joined));
        c.push(("next start: .reconnect is read once (removed)".into(), !reconnect.exists()));
        run.clean();

        c.push(empty_server("next start, answered", port));
        say!("   -- next start, card answered");
        let run = Run::new(self, root, "next-ok", display);
        run.write_answers(&format!("reconnect=ok\ntyped={PASSWORD}\n"));
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        fs::create_dir_all(reconnect.parent().unwrap()).unwrap();
        fs::write(
            reconnect,
            format!("format = 1\naddress = \"{addr}\"\nserver_name = \"xtask\"\npassword_required = true\ntime = {now}\n"),
        )
        .unwrap();
        let mut g = run.start_game("name xtask_e2e\n", &[]);
        let joined = wait_in_game(port, Duration::from_secs(if root.os == Os::Windows { 120 } else { 60 }));
        let _ = g.kill();
        let _ = g.wait();
        run.kill_games();
        root.cleanup();
        run.keep_engine_log();
        c.push(("next start: clicked (password typed), the game joined the passworded server".into(), joined));
        let _ = fs::remove_file(reconnect);
        run.clean();
        c
    }
}

/// `n` lines of 20 `wait`s (FWGS's console buffer is 32 KB; see AGENTS.md).
fn waits(n: usize) -> String {
    format!("{}\n", "wait;".repeat(20)).repeat(n)
}

pub(crate) fn fake_name(t: &Target) -> &'static str {
    if t.os == Os::Windows { "ts-launcher-fake.exe" } else { "ts-launcher-fake" }
}

fn wait_exit(c: &mut Child, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if c.try_wait().ok().flatten().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

fn wait_players(port: u16, pred: impl Fn(u8) -> bool, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if a2s::info("127.0.0.1", port).is_ok_and(|i| pred(i.players)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Waits for the server on `port` to have no players (a killed game lingers until `sv_timeout`),
/// so that a player seen later is the scenario's own game.
fn empty_server(scenario: &str, port: u16) -> (String, bool) {
    (format!("{scenario}: the server on port {port} was empty first"), wait_players(port, |p| p == 0, Duration::from_secs(60)))
}

/// Waits for an `xtask_e2e` in the game on `port` (see the module docs).
fn wait_in_game(port: u16, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if a2s::rcon("127.0.0.1", port, false, RCON, "status").is_ok_and(|s| in_game(&s)) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Whether FWGS's `status` (`SV_Status_f`: `# score ping dev lastmsg qport useragent name
/// address`) lists a spawned `xtask_e2e` heard from in the last 3 s.
fn in_game(status: &str) -> bool {
    status.lines().any(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        w.len() > 5
            && w[0].parse::<u32>().is_ok()
            && w[2].parse::<i32>().is_ok()
            && w[4].parse::<f64>().is_ok_and(|last| last < 3.0)
            && w.contains(&"xtask_e2e")
    })
}

/// `~/scratch/xvfb/usr/bin/Xvfb :87` (or `XVFB`); the child and the display name.
fn start_xvfb() -> Option<(Child, String)> {
    let home = PathBuf::from(std::env::var("HOME").ok()?);
    let bin = std::env::var_os("XVFB").map(PathBuf::from).unwrap_or_else(|| home.join("scratch/xvfb/usr/bin/Xvfb"));
    let lib = bin.parent()?.parent()?.join("lib");
    let display = ":87".to_owned();
    let child = Command::new(&bin)
        .env("LD_LIBRARY_PATH", lib)
        .args([display.as_str(), "-screen", "0", "1024x768x24", "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    std::thread::sleep(Duration::from_secs(2));
    Some((child, display))
}

/// One scenario's files and processes.
struct Run<'a> {
    ctx: &'a Ctx,
    root: &'a Root,
    tag: &'static str,
    display: Option<&'a str>,
}

impl<'a> Run<'a> {
    fn new(ctx: &'a Ctx, root: &'a Root, tag: &'static str, display: Option<&'a str>) -> Run<'a> {
        let r = Run { ctx, root, tag, display };
        let _ = fs::remove_file(r.fake_log_path());
        let _ = fs::remove_file(r.answers_path().with_extension("txt.shown"));
        r
    }

    fn dir(&self) -> PathBuf {
        self.ctx.repo.join("target").join("relaunch")
    }

    fn fake_exe(&self) -> PathBuf {
        self.root.path.join(LAUNCHER_DIR).join(fake_name(&self.root.target()))
    }

    fn fake_log_path(&self) -> PathBuf {
        self.dir().join(format!("{}-{}-launcher.log", self.root.name, self.tag))
    }

    fn answers_path(&self) -> PathBuf {
        self.dir().join(format!("{}-{}-cards.txt", self.root.name, self.tag))
    }

    fn fake_log(&self) -> String {
        fs::read_to_string(self.fake_log_path()).unwrap_or_default().replace('\r', "")
    }

    fn shown(&self) -> String {
        fs::read_to_string(self.answers_path().with_extension("txt.shown")).unwrap_or_default()
    }

    fn write_answers(&self, text: &str) {
        fs::create_dir_all(self.dir()).unwrap();
        fs::write(self.answers_path(), text).unwrap();
    }

    /// `ts-launcher-fake.cfg`: the log, the IPC answer, and how to start the game again.
    fn write_fake_cfg(&self, answer: &str) {
        let r = self.root;
        let mut cfg = format!("log={}\nanswer={answer}\ncwd={}\n", native(r, &self.fake_log_path()), native(r, &r.path));
        if r.os == Os::Windows {
            cfg += &format!("game={}\n", native(r, &r.path.join("xash3d.exe")));
        } else {
            cfg += &format!("game={}\n", r.path.join("xash3d").display());
        }
        for a in ["-game", "ts", "-dev", "2", "-windowed", "-width", "640", "-height", "480"] {
            cfg += &format!("arg={a}\n");
        }
        fs::write(self.fake_exe().with_file_name("ts-launcher-fake.cfg"), cfg).unwrap();
    }

    fn endpoint(&self) -> String {
        if self.root.os == Os::Windows {
            format!(r"\\.\pipe\ts-launcher-e2e-{}", std::process::id())
        } else {
            let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
            format!("{dir}/ts-launcher-e2e-{}.sock", std::process::id())
        }
    }

    fn apply_env(&self, c: &mut Command) {
        c.env("TSRS_E2E_CARDS", native(self.root, &self.answers_path()));
        if let Some(d) = self.display {
            c.env("DISPLAY", d).env_remove("WAYLAND_DISPLAY").env("SDL_VIDEODRIVER", "x11");
        }
        if self.root.os == Os::Windows {
            c.env("WINEPREFIX", roots::wineprefix()).env("WINEDEBUG", "-all");
        }
    }

    fn start_fake_serve(&self, ep: &str) -> Child {
        let mut c = if self.root.os == Os::Windows {
            let mut c = Command::new("wine");
            c.arg(self.fake_exe());
            c
        } else {
            Command::new(self.fake_exe())
        };
        self.apply_env(&mut c);
        let out = fs::File::create(self.dir().join(format!("{}-{}-launcher.out", self.root.name, self.tag))).unwrap();
        c.current_dir(&self.root.path)
            .args(["serve", "--endpoint", ep])
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .expect("start ts-launcher-fake")
    }

    fn wait_log(&self, what: &str, limit: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < limit {
            if self.fake_log().contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        false
    }

    /// The first game: runs `script` (in `<root>/ts/xtask_relaunch.cfg`) with `env` added.
    fn start_game(&self, script: &str, env: &[(String, String)]) -> Child {
        let r = self.root;
        fs::write(r.path.join("ts").join(CLIENT_CFG), script).unwrap();
        let args = ["-game", "ts", "-dev", "2", "-windowed", "-width", "640", "-height", "480", "+exec", CLIENT_CFG];
        let mut c = if r.os == Os::Windows {
            let mut c = Command::new("wine");
            c.arg("xash3d.exe").arg("-log");
            c
        } else {
            Command::new(r.path.join("xash3d"))
        };
        self.apply_env(&mut c);
        for (k, v) in env {
            c.env(k, v);
        }
        let out = fs::File::create(self.dir().join(format!("{}-{}-game.log", r.name, self.tag))).unwrap();
        say!("   game log: {}", self.dir().join(format!("{}-{}-game.log", r.name, self.tag)).display());
        c.current_dir(&r.path).args(args).stdout(out.try_clone().unwrap()).stderr(out).spawn().expect("start the game")
    }

    /// Stops the games this scenario started indirectly (the fake launcher's or the self-relaunch's):
    /// on Linux every process running this root's `xash3d`, under Wine the prefix's processes.
    /// `SIGKILL`: SDL turns `SIGTERM` into `SDL_QUIT`, which FWGS hands to the menu's quit
    /// question ("CL_RequestQuit: ... passing to the menu"), so the game would stay connected.
    fn kill_games(&self) {
        if self.root.os == Os::Windows {
            roots::wine_cleanup();
            return;
        }
        let Ok(exe) = self.root.path.join("xash3d").canonicalize() else { return };
        for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if fs::read_link(e.path().join("exe")).is_ok_and(|p| p == exe) {
                let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    /// Under Wine, `-log` writes `<root>/engine.log` (each start truncates it): keep this
    /// scenario's last one.
    fn keep_engine_log(&self) {
        if self.root.os == Os::Windows {
            let _ = fs::copy(self.root.path.join("engine.log"), self.dir().join(format!("{}-{}-engine.log", self.root.name, self.tag)));
        }
    }

    fn clean(&self) {
        let _ = fs::remove_file(self.root.path.join("ts").join(CLIENT_CFG));
        let _ = fs::remove_file(self.root.path.join("ts").join("xtask_relaunch_done"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn in_game_needs_a_spawned_recent_player() {
        let head = "map: ts_dojo\n# score ping dev  lastmsg qport useragent\t\tname\t\taddress\n";
        let line = |state: &str, last: &str| {
            format!("{head} 1     0 {state}    m {last}  1234 0.21 (linux-amd64 1)\txtask_e2e\t127.0.0.1:27005\n\n")
        };
        assert!(super::in_game(&line("      12", "0.01000")));
        assert!(!super::in_game(&line("Connect ", "0.01000")));
        assert!(!super::in_game(&line("Spawning", "0.01000")));
        assert!(!super::in_game(&line("Zombie  ", "0.01000")));
        // A killed game, still listed until sv_timeout.
        assert!(!super::in_game(&line("      12", "8.50000")));
        assert!(!super::in_game(head));
    }
}
