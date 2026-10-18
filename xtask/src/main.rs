//! Build, package, install and validate the TS port.
//!
//! ```text
//! cargo xtask build   [--release]                 build all four targets (after checking that the
//!                                                 game crates don't use goldsrc's engine-call macros)
//! cargo xtask lint                                engine-macro check + `cargo clippy --workspace
//!                                                 --all-targets -- -D warnings`, all four targets
//!                                                 (Windows-only code included)
//! cargo xtask dist    [--release]                 dist/<platform>/ts/: libraries + game files
//!
//! `--release` builds the release profile and installs stripped copies of the libraries;
//! `--release-debug` installs them as built, with debug info. The code is the same, so the
//! debug libraries symbolize crash addresses from the stripped ones (`addr2line -e`).
//! cargo xtask package-xash-win                    `dist --release` for windows-i686, then
//!                                                 ~/ts-xash-win-release.tar.gz: FWGS win32 engine +
//!                                                 stock TS 3.0 + the port (see package.rs)
//! cargo xtask install [--release] <root>... | all assemble <root>/ts and install libraries
//! cargo xtask server  [--release] <root> [--map M] [--bots N] [--seconds S] [--port P]
//!                                                 run a dedicated server and check it
//! cargo xtask test    [--release] [<root>...]     dedicated server + bots on every root, in parallel
//! cargo xtask play    [--release] [<fwgs-root>...]
//!                                                 gameplay on a listen server (join, buy, spawn,
//!                                                 power-up, grenade, kills, kick, bot fights), in
//!                                                 parallel on fwgs and fwgs-win
//! cargo xtask meck    [--release] [<fwgs-root>...]
//!                                                 grenades on MecklenburgV_Rain_Final: a grenade
//!                                                 kill and broken func_breakables (also run by play)
//! cargo xtask bots    [--release] [<fwgs-root>...]
//!                                                 TS's bot AI on ts_dojo: testpath, four addbot bots
//!                                                 roaming the node graph and fighting, botlearn
//! cargo xtask plugins [--release] [<root>...]     the `tsmod_*` loader (stub plugin + fixtures it
//!                                                 must skip) and the stub's bot backend on a
//!                                                 dedicated server, every root in parallel
//!                                                 (also run by play)
//! cargo xtask plugin-approve <tspkg|package dir|cl_dlls/tsmod_<name><suffix>>... [--root R | --game-dir D] [--platform P]
//!                                                 approve a local client plugin build in the
//!                                                 game's cl_plugins/approved.toml, as the start-up
//!                                                 box's "Allow" would (plugin_approve.rs)
//! cargo xtask ts21    [--release] [--no-clients] [<root>...]
//!                                                 TS 2.1 mode on a dedicated server per root, by
//!                                                 rcon: reloads, LMS, The One, teamplay, round
//!                                                 TDM, objective rounds, a large map; then the
//!                                                 port's and the original TS client on the Linux
//!                                                 Steam roots (see ts21.rs; not in `all`)
//! cargo xtask version [--release] [<root>...]
//!                                                 the port client's version check at connect
//!                                                 (sv_client_version, sv_client_require_port) on
//!                                                 each Linux root's dedicated server with the FWGS
//!                                                 port client and the original TS client (see
//!                                                 version.rs; not in `all`)
//! cargo xtask relaunch [--release] [--xvfb] [--only <scenario>,...] [<fwgs-root>...]
//!                                                 restart and reconnect end to end with the e2e
//!                                                 client and ts-launcher-fake: IPC, relaunch
//!                                                 arguments, self-relaunch, quit and inform, the
//!                                                 next start (see relaunch.rs; not in `all`)
//! cargo xtask tspkg   [--release] [<root>...]
//!                                                 server offers of client plugins (TSPkg, tspkg
//!                                                 replies, op 5) on each Linux root's dedicated
//!                                                 server with the FWGS port client (see tspkg.rs;
//!                                                 not in `all`)
//! cargo xtask plugin-offer [--required] <tspkg>   check a built package, write the files a client
//!                                                 fetches before it (.manifest.toml, .manifest.sig,
//!                                                 .publisher.cert) beside it, and print its
//!                                                 plugin_offers.toml entry
//! cargo xtask all     [--release]                 test, then play, then steam (only when Steam is
//!                                                 running and logged in; skipped with a note otherwise)
//! cargo xtask client  [--release] <server-root> [--map M] [--port P]
//!                                                 FWGS client (Rust client library) joins a dedicated
//!                                                 server on <server-root>; screenshot in target/
//! cargo xtask steam   [--release] [listen | <server-root>...]
//!                                                 the Steam Half-Life 25 client, installed into
//!                                                 Half-Life/ts: `play`'s scenes plus changelevel on
//!                                                 a listen server, or join/chat/kill/respawn/
//!                                                 changelevel on hlds25-linux / rehlds-linux; the
//!                                                 friends status is invisible meanwhile (steam.rs)
//! cargo xtask roots                               list engine roots and whether they exist
//! ```
//!
//! Engine roots default to ~/games/<dir> and can be overridden with `TS_ROOT_<NAME>`
//! (e.g. `TS_ROOT_REHLDS_LINUX=/srv/hlds`). TS content comes from `TS_DIR` (default
//! `../ts_custom`, TS 3.0 with custom maps such as MecklenburgV_Rain_Final; looked up from the
//! checkout's parent directories, so worktrees find it too): the installed game directory links to
//! its maps, models and sounds without modifying it. The stock content (`../ts_original`,
//! `TS_ORIGINAL_DIR`) is only for `package-xash-win`.

mod a2s;
mod client_plugins;
mod lint;
mod package;
mod plugin_approve;
mod plugins;
mod relaunch;
mod roots;
mod steam;
mod ts21;
mod tspkg;
mod tspkg_client;
mod version;

use roots::{Engine, Os, Root, Target};
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use std::time::{Duration, Instant};
use std::{env, fs};

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let profile = if take_flag(&mut args, "--release-debug") {
        Profile::ReleaseDebug
    } else if take_flag(&mut args, "--release") {
        Profile::Release
    } else {
        Profile::Dev
    };
    let Some(cmd) = (!args.is_empty()).then(|| args.remove(0)) else { usage() };
    let ctx = Ctx { profile, repo: repo_root() };
    if launches_games(&cmd) {
        require_display();
    }
    match cmd.as_str() {
        "build" => ctx.build(&Target::ALL),
        "lint" => {
            if !lint::run(&ctx.repo) {
                exit(1);
            }
        }
        "dist" => ctx.dist(&Target::ALL),
        "package-xash-win" => {
            let ctx = Ctx { profile: Profile::Release, ..ctx };
            ctx.dist(&[Target::WINDOWS_I686]);
            let (commit, date) = package::commit(&ctx.repo);
            let dist_ts = ctx.repo.join("dist").join(Target::WINDOWS_I686.platform).join("ts");
            let stage = ctx.repo.join("target").join("package-xash-win");
            package::package(&package::Paths::from_env(&ctx.repo), &dist_ts, &stage, &commit, &date);
        }
        "install" => {
            for r in select_roots(&args) {
                ctx.install(&r);
            }
        }
        "server" => {
            let map = take_opt(&mut args, "--map").unwrap_or_else(|| "ts_arena".into());
            let bots: u32 = take_opt(&mut args, "--bots").map_or(0, |v| v.parse().unwrap());
            let secs: u64 = take_opt(&mut args, "--seconds").map_or(20, |v| v.parse().unwrap());
            let port: u16 = take_opt(&mut args, "--port").map_or(27115, |v| v.parse().unwrap());
            let root = select_roots(&args).into_iter().next().unwrap_or_else(|| usage());
            if !ctx.server(&root, &map, bots, secs, port) {
                exit(1);
            }
        }
        "test" => {
            let map = take_opt(&mut args, "--map").unwrap_or_else(|| "ts_arena".into());
            if !ctx.test_all(&select_roots(&args), &map) {
                exit(1);
            }
        }
        "meck" => {
            if !ctx.meck_all(&play_roots(&args)) {
                exit(1);
            }
        }
        "bots" => {
            if !ctx.bots_all(&play_roots(&args)) {
                exit(1);
            }
        }
        "plugins" => {
            if !ctx.plugins_all(&select_roots(&args)) {
                exit(1);
            }
        }
        "client-plugins" => {
            if !ctx.client_plugins_all(&play_roots(&args)) {
                exit(1);
            }
        }
        "plugin-approve" => {
            if !plugin_approve::main(args) {
                exit(1);
            }
        }
        "ts21" => {
            let clients = !take_flag(&mut args, "--no-clients");
            if !ctx.ts21_all(&select_roots(&args), clients) {
                exit(1);
            }
        }
        "version" => {
            if !ctx.version_all(&select_roots(&args)) {
                exit(1);
            }
        }
        "relaunch" => {
            if !ctx.relaunch_all(&args) {
                exit(1);
            }
        }
        "tspkg" => {
            if !ctx.tspkg_all(&select_roots(&args)) {
                exit(1);
            }
        }
        "plugin-offer" => {
            let required = take_flag(&mut args, "--required");
            let Some(path) = args.first() else { usage() };
            match tspkg::plugin_offer(Path::new(path), required) {
                Ok(entry) => print!("{entry}"),
                Err(e) => {
                    eprintln!("plugin-offer: {e}");
                    exit(1);
                }
            }
        }
        "play" => {
            if !ctx.play_all(&play_roots(&args)) {
                exit(1);
            }
        }
        "all" => {
            let servers = ctx.test_all(&Root::all(), "ts_arena");
            let play = ctx.play_all(&play_roots(&[]));
            let steam = steam::all_step(&ctx);
            say!("== all: dedicated servers {}, gameplay {}, Steam client {}", pass(servers), pass(play), steam.map_or("skipped", pass));
            if !(servers && play && steam.unwrap_or(true)) {
                exit(1);
            }
        }
        "client" => {
            let map = take_opt(&mut args, "--map").unwrap_or_else(|| "ts_awaken".into());
            let port: u16 = take_opt(&mut args, "--port").map_or(27150, |v| v.parse().unwrap());
            let roots = select_roots(&args);
            let fwgs = Root::all().into_iter().find(|r| r.name == "fwgs").unwrap();
            let mut failed = Vec::new();
            for (i, r) in roots.into_iter().filter(|r| r.exists()).enumerate() {
                ctx.install(&r);
                if !ctx.client(&r, &fwgs, &map, port + i as u16) {
                    failed.push(r.name);
                }
            }
            if !failed.is_empty() {
                eprintln!("FAILED: {failed:?}");
                exit(1);
            }
        }
        "steam" => {
            if !ctx.steam(&args) {
                exit(1);
            }
        }
        "roots" => {
            for r in Root::all() {
                say!(
                    "{:14} {:7} {:?} {} {}",
                    r.name,
                    format!("{:?}", r.os),
                    r.engine,
                    r.path.display(),
                    if r.exists() { "ok" } else { "missing" }
                );
            }
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: cargo xtask [--release|--release-debug] <build|lint|dist|package-xash-win|install|server|test|play|meck|bots|plugins|client-plugins|plugin-approve|ts21|version|relaunch|tspkg|plugin-offer|all|client|steam|roots> ... (see xtask/src/main.rs)"
    );
    exit(2)
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

fn take_opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.remove(i);
    (i < args.len()).then(|| args.remove(i))
}

fn select_roots(names: &[String]) -> Vec<Root> {
    if names.is_empty() || names.iter().any(|n| n == "all") {
        return Root::all();
    }
    names
        .iter()
        .map(|n| {
            Root::all().into_iter().find(|r| r.name == n).unwrap_or_else(|| {
                eprintln!("unknown root {n}; see `cargo xtask roots`");
                exit(2)
            })
        })
        .collect()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn ts_dir(repo: &Path) -> PathBuf {
    env::var_os("TS_DIR").map(PathBuf::from).unwrap_or_else(|| package::sibling(repo, "ts_custom"))
}

/// `Dev`: the dev profile. `Release` and `ReleaseDebug` share one release-profile build (with
/// debug info); `Release` installs stripped copies of its libraries.
#[derive(Clone, Copy, PartialEq)]
enum Profile {
    Dev,
    Release,
    ReleaseDebug,
}

impl Profile {
    /// The output directory under target/<triple>/.
    fn dir(self) -> &'static str {
        if self == Profile::Dev { "debug" } else { "release" }
    }
}

struct Ctx {
    profile: Profile,
    repo: PathBuf,
}

thread_local! {
    /// Output of the check running on this thread, printed as one block when it finishes (so
    /// parallel roots don't interleave). `None`: print directly.
    static CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// `println!` that goes to the current thread's capture buffer while a parallel check runs.
#[macro_export]
macro_rules! say {
    ($($t:tt)*) => { $crate::say_line(format!($($t)*)) };
}

fn say_line(s: String) {
    CAPTURE.with(|c| match &mut *c.borrow_mut() {
        Some(buf) => {
            buf.push_str(&s);
            buf.push('\n');
        }
        None => println!("{s}"),
    })
}

fn pass(ok: bool) -> &'static str {
    if ok { "PASS" } else { "FAIL" }
}

/// FWGS roots for `play` (default: both).
fn play_roots(names: &[String]) -> Vec<Root> {
    let names: Vec<String> = if names.is_empty() { vec!["fwgs".into(), "fwgs-win".into()] } else { names.to_vec() };
    select_roots(&names).into_iter().filter(|r| r.engine == Engine::Fwgs).collect()
}

impl Ctx {
    /// Installs every existing root (one build per target), then runs `check(root, index)` for
    /// all of them in parallel. Each root's output is printed as a block when it finishes. Windows
    /// roots share one Wine prefix, so per-root `wineserver -k` is skipped and done once at the end.
    fn parallel(&self, roots: &[Root], check: impl Fn(&Root, usize) -> bool + Sync) -> bool {
        let roots: Vec<&Root> = roots
            .iter()
            .filter(|r| {
                let ok = r.exists();
                if !ok {
                    say!("== {}: skipped ({} missing)", r.name, r.path.display());
                }
                ok
            })
            .collect();
        for r in &roots {
            self.install(r);
        }
        let start = Instant::now();
        roots::set_parallel(true);
        let results: Vec<(&'static str, bool)> = std::thread::scope(|s| {
            let handles: Vec<_> = roots
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let check = &check;
                    s.spawn(move || {
                        CAPTURE.with(|c| *c.borrow_mut() = Some(String::new()));
                        let ok = check(r, i);
                        let out = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
                        print!("{out}");
                        (r.name, ok)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or(("?", false))).collect()
        });
        roots::set_parallel(false);
        roots::wine_cleanup();
        let failed: Vec<&str> = results.iter().filter(|r| !r.1).map(|r| r.0).collect();
        say!(
            "== {} root(s) in {:.0}s: {}",
            results.len(),
            start.elapsed().as_secs_f32(),
            if failed.is_empty() { "all passed".to_owned() } else { format!("FAILED {failed:?}") }
        );
        failed.is_empty()
    }

    /// `test`: a dedicated server with 2 bots for 15 s on every root.
    fn test_all(&self, roots: &[Root], map: &str) -> bool {
        self.parallel(roots, |r, i| self.server(r, map, 2, 15, 27115 + i as u16))
    }

    /// `play`: the gameplay check on each FWGS root.
    fn play_all(&self, roots: &[Root]) -> bool {
        self.parallel(roots, |r, i| {
            self.play(r, 27215 + 10 * i as u16) & self.meck(r, 27315 + 10 * i as u16) & self.ts_bots(r, 27415 + 10 * i as u16)
        })
    }

    fn meck_all(&self, roots: &[Root]) -> bool {
        self.parallel(roots, |r, i| self.meck(r, 27315 + 10 * i as u16))
    }

    fn bots_all(&self, roots: &[Root]) -> bool {
        self.parallel(roots, |r, i| self.ts_bots(r, 27415 + 10 * i as u16))
    }
}

impl Ctx {
    fn profile(&self) -> &'static str {
        self.profile.dir()
    }

    fn build(&self, targets: &[Target]) {
        let hits = lint::engine_macro_uses(&self.repo);
        if !hits.is_empty() {
            eprintln!("engine-call macros in game crates (AGENTS.md rule 2: add a safe wrapper to goldsrc):");
            for h in hits {
                eprintln!("  {h}");
            }
            exit(1);
        }
        let leaks = lint::ruleset_leaks(&self.repo);
        if !leaks.is_empty() {
            eprintln!("a ruleset named outside its plugin (issues #100, #200: keep each ruleset in its crate):");
            for h in leaks {
                eprintln!("  {h}");
            }
            exit(1);
        }
        for t in targets {
            let mut c = Command::new(env::var("CARGO").unwrap_or("cargo".into()));
            // `tsmod-test` is the stub plugin (`cargo xtask plugins`) and `tsmod-client-test` the
            // stub client extension (`cargo xtask client-plugins`); dist/ ships neither.
            c.current_dir(&self.repo).args([
                "build",
                "-p",
                "ts-server-dll",
                "-p",
                "ts-client-dll",
                "-p",
                "tsmod-test",
                "-p",
                "tsmod-client-test",
                "--target",
                t.triple,
            ]);
            if self.profile != Profile::Dev {
                c.arg("--release");
            }
            say!("== cargo build --target {}", t.triple);
            if !c.status().expect("cargo").success() {
                exit(1);
            }
        }
    }

    fn artifact(&self, t: &Target, server: bool) -> PathBuf {
        let dir = self.repo.join("target").join(t.triple).join(self.profile());
        let name = match (t.os, server) {
            (Os::Linux, true) => "libts.so",
            (Os::Linux, false) => "libclient.so",
            (Os::Windows, true) => "ts.dll",
            (Os::Windows, false) => "client.dll",
        };
        dir.join(name)
    }

    /// Copies the libraries for `t` into `gamedir` under the names each engine loads.
    fn install_libs(&self, t: &Target, gamedir: &Path) {
        let server = gamedir.join("dlls").join(t.server_name);
        let client = gamedir.join("cl_dlls").join(t.client_name);
        copy(&self.artifact(t, true), &server);
        copy(&self.artifact(t, false), &client);
        for lib in [&server, &client] {
            if self.profile == Profile::Release {
                strip(t, lib);
            }
            if t.os == Os::Windows {
                msvc_style_export_section(lib);
            }
        }
        // Stock HLDS (8684 and HL25) strips "_i386" from gamedll_linux and loads dlls/ts.so;
        // ReHLDS and old engines load the name as written. Install both.
        if t.server_name == "ts_i386.so" {
            copy(&server, &gamedir.join("dlls/ts.so"));
        }
    }

    fn dist(&self, targets: &[Target]) {
        self.build(targets);
        for t in targets {
            let gamedir = self.repo.join("dist").join(t.platform).join("ts");
            copy_dir(&self.repo.join("game/ts"), &gamedir);
            self.install_libs(t, &gamedir);
        }
        say!("dist/ ready: copy dist/<platform>/ts over a TS 3.0 install's ts/ directory");
    }

    /// Assembles `<root>/ts`: links TS content, copies writable config files, installs our game
    /// files and libraries.
    fn install(&self, root: &Root) {
        if !root.exists() {
            eprintln!("== {}: {} missing, skipped", root.name, root.path.display());
            return;
        }
        let t = root.target();
        self.build(&[t]);
        let gamedir = root.path.join("ts");
        link_ts_content(&ts_dir(&self.repo), &gamedir);
        copy_dir(&self.repo.join("game/ts"), &gamedir);
        self.install_libs(&t, &gamedir);
        // A `plugins` run cut short may have left its stub and fixtures: other runs go without.
        plugins::remove_fixtures(&gamedir.join("dlls"));
        client_plugins::remove_fixtures(&gamedir.join("cl_dlls"));
        client_plugins::remove_plugin_fixtures(&gamedir);
        say!("== {}: installed into {}", root.name, gamedir.display());
    }

    /// Starts a dedicated server, waits for it to answer A2S_INFO, optionally checks that `bots`
    /// bots joined, watches the log for panics, then stops it.
    fn server(&self, root: &Root, map: &str, bots: u32, secs: u64, port: u16) -> bool {
        say!("== {}: dedicated server, map {map}, {bots} bots, {secs}s, port {port}", root.name);
        let log = root.log_path();
        let _ = fs::remove_file(&log);
        let mut cmd = root.server_command(map, port, bots);
        let stdout = fs::File::create(self.repo.join("target").join(format!("server-{}.log", root.name))).unwrap();
        cmd.stdout(stdout.try_clone().unwrap()).stderr(stdout);
        let mut child = cmd.spawn().expect("start server");
        let start = Instant::now();
        let mut info = None;
        // Windows servers run under Wine and take noticeably longer to come up.
        let limit = Duration::from_secs(if root.os == Os::Windows { 120 } else { 60 });
        while start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(500));
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            if let Ok(i) = a2s::info("127.0.0.1", port) {
                info = Some(i);
                break;
            }
        }
        let mut ok = true;
        match &info {
            Some(i) => say!("   up after {:.1}s: {i}", start.elapsed().as_secs_f32()),
            None => {
                say!("   FAIL: no A2S_INFO reply within {}s", limit.as_secs());
                ok = false;
            }
        }
        if ok {
            std::thread::sleep(Duration::from_secs(secs));
            // HL25 rate-limits status queries per address: retry a few times before failing.
            let mut reply = a2s::info("127.0.0.1", port);
            for _ in 0..5 {
                if reply.is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(2));
                reply = a2s::info("127.0.0.1", port);
            }
            match reply {
                Ok(i) => {
                    say!("   after {secs}s: {i}");
                    let players = a2s::players("127.0.0.1", port).unwrap_or_default();
                    let list: Vec<String> = players.iter().map(|(n, f)| format!("{n}={f}")).collect();
                    say!("   scores: {}", list.join(", "));
                    // `xtask_server.cfg` runs `addbot`: the port adds the bot on a dedicated server
                    // too (TS only says "Bots only in listen servers."). Its name comes from
                    // `bots.txt`, not the quota's "Bot " names. GoldSrc prints TS's "connected
                    // with skill" only with `developer`, the old FWGS query lists no bots.
                    let named = players.iter().any(|(n, _)| !n.starts_with("Bot "));
                    if !named && !root.read_log(&self.repo).contains("connected with skill") {
                        say!("   FAIL: addbot added no bot on a dedicated server");
                        ok = false;
                    }
                    if i.map != map {
                        say!("   FAIL: map is {:?}, expected {map:?}", i.map);
                        ok = false;
                    }
                    if i.players < bots as u8 {
                        // The Xash `info` reply (old FWGS) doesn't count fake clients: use the log.
                        let added = root.read_log(&self.repo).matches("[ts] bots: added").count();
                        if i.game.is_empty() && added >= bots as usize {
                            say!("   {added} bots joined (from the log; this engine's query doesn't count bots)");
                        } else {
                            say!("   FAIL: {} players, expected {bots} bots", i.players);
                            ok = false;
                        }
                    }
                }
                Err(e) => {
                    say!("   FAIL: server stopped answering: {e}");
                    ok = false;
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        root.cleanup();
        let text = root.read_log(&self.repo);
        for line in text.lines().filter(|l| l.contains("[ts]") || l.contains("PANIC") || l.contains("FATAL")) {
            say!("   | {}", line.trim_end());
        }
        if text.contains("PANIC") || text.contains("FATAL ERROR") || text.contains("Segmentation") {
            say!("   FAIL: panic or fatal error in the log");
            ok = false;
        }
        if !text.contains("[ts] GameInit") {
            say!("   FAIL: the Rust server library didn't load (no GameInit line)");
            ok = false;
        }
        say!("   {}", if ok { "PASS" } else { "FAIL" });
        ok
    }
}

impl Ctx {
    /// Runs `server_root`'s dedicated server (2 bots) on a thread and joins it with the FWGS
    /// client from `client_root`. Passes when the client connects and the server check passes.
    fn client(&self, server_root: &Root, client_root: &Root, map: &str, port: u16) -> bool {
        self.install(client_root);
        let srv = Ctx { profile: self.profile, repo: self.repo.clone() };
        let (sr, m) = (server_root.clone(), map.to_owned());
        let server = std::thread::spawn(move || srv.server(&sr, &m, 2, 60, port));
        let start = Instant::now();
        while a2s::info("127.0.0.1", port).is_err() && start.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_secs(3));

        // GoldSrc servers need the GoldSrc protocol; FWGS servers speak FWGS's own.
        let proto = if server_root.engine == Engine::Fwgs { "" } else { " gs" };
        let w = "wait;".repeat(20);
        let waits = format!("{w}\n").repeat(30);
        let cfg = format!("connect 127.0.0.1:{port}{proto}\n{waits}echo XTASK: in game\nscreenshot\n{}quit\n", format!("{w}\n").repeat(3));
        let gamedir = client_root.path.join("ts");
        fs::write(gamedir.join("xtask_client.cfg"), cfg).unwrap();
        let _ = fs::remove_dir_all(gamedir.join("scrshots"));
        let log_path = self.repo.join("target").join(format!("client-{}.log", server_root.name));
        let log = fs::File::create(&log_path).unwrap();
        let mut child = Command::new(client_root.path.join("xash3d"))
            .current_dir(&client_root.path)
            .args(["-game", "ts", "-dev", "2", "-windowed", "-width", "1024", "-height", "768", "+exec", "xtask_client.cfg"])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start client");
        let t = Instant::now();
        while child.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = child.kill();
        let _ = child.wait();

        let text = String::from_utf8_lossy(&fs::read(&log_path).unwrap_or_default()).replace('\r', "");
        let connected = text.contains("client connected");
        let in_game = text.contains("XTASK: in game");
        let panicked = text.contains("PANIC");
        let shot = fs::read_dir(gamedir.join("scrshots")).ok().and_then(|d| d.flatten().next()).map(|e| e.path());
        if let Some(p) = &shot {
            let dest = self.repo.join("target").join(format!("client-{}.png", server_root.name));
            let _ = fs::copy(p, &dest);
            say!("   client screenshot: {}", dest.display());
        }
        // `echo` output isn't always mirrored to stdout, so the screenshot is the in-game evidence.
        let ok = connected && !panicked && shot.is_some();
        say!("   client: connected={connected} in_game={in_game} panic={panicked} screenshot={}", shot.is_some());
        let server_ok = server.join().unwrap();
        say!("   {} (client + server)", if ok && server_ok { "PASS" } else { "FAIL" });
        ok && server_ok
    }
}

/// Game time runs this many times faster than real time in scripted runs (`sys_timescale`). The
/// scripts pin `fps_max 100`, and FWGS caps frames by real time, so a frame is 10 ms real and
/// `TIMESCALE * 10` ms of game time.
const TIMESCALE: f32 = 2.0;

/// Wait `n` seconds of **game** time. `script_start` defines `w10` (10 frames) and `w100` (100
/// frames); plain `wait` chains would overflow the engine's 32 KB command buffer. Under Wine the
/// frame rate can drop below 100, which only makes the waits longer in game time.
fn secs(n: f32) -> String {
    let frames = (n * 100.0 / TIMESCALE).round() as usize;
    "w100;".repeat(frames / 100) + &"w10;".repeat(frames % 100 / 10) + &"wait;".repeat(frames % 10) + "\n"
}

fn shot(tag: &str) -> String {
    format!("echo XTASK: {tag}\nscreenshot\n")
}

/// Script prologue: cheats for the dev commands, scripted input past the client's anti-script
/// gate (`ts_dev_scriptinput 1`), faster game time, then the map. `lservercfgfile ""` stops the
/// game from queueing the root's `listenserver.cfg` on every map (TS's `CHalfLifeMultiplay`
/// constructor does, and TS's shipped file adds four bots and turns `sv_cheats` off). A `motdfile`
/// that doesn't exist sends no MOTD (only the server name), so the MOTD window, which a script
/// can't close, doesn't cover the screenshots.
fn script_start(map: &str) -> String {
    format!(
        "// Written by cargo xtask.\nalias w10 \"{}\"\nalias w100 \"{}\"\nfps_max 100\nsys_timescale {TIMESCALE}\nsv_cheats 1\nts_dev_scriptinput 1\nmaxplayers 8\nlservercfgfile \"\"\nmotdfile xtask_no_motd.txt\nmap {map}\n",
        "wait;".repeat(10),
        "w10;".repeat(10)
    )
}

/// Join flow: wait for the buy menu (open on join with a random first loadout), Reset -> Buy ->
/// Rifles -> AK47 -> scope -> add -> back -> done, then click `+attack` after the respawn wait.
///
/// The keys go through twice, each time after `buy`, which opens the menu again while the player
/// is an observer (TS's `buy`, `0x1004fda0`; a no-op while it is open). A window resize makes the
/// engine call `HUD_VidInit`, and that closes the menu, as TS's (`CHudTSWeapons::VidInit`
/// `0x1004cb50` re-runs the menu's `Init` `0x10036200`). Under Wine the compositor resizes the
/// FWGS window a second or two after the join, so the keys sometimes went to a closed menu and the
/// host spawned with nothing (issue #102). The second pass submits the same loadout again, so a
/// resize at any one point no longer loses the purchase. A menu reopened after a resize starts
/// from a new random loadout (issue #189), which each pass's first key (4, Reset) discards.
fn script_join_and_spawn() -> String {
    let mut cfg = secs(8.0) + &shot("joined");
    for _ in 0..2 {
        cfg += "buy\n";
        for key in [4, 2, 3, 2, 5, 1, 10, 1] {
            cfg += &format!("slot{key}\n{}", secs(0.5));
        }
    }
    cfg += &secs(5.0);
    cfg += &click();
    cfg + &secs(3.0)
}

fn click() -> String {
    "echo XTASK: click\n+attack\nwait;wait;wait;wait;wait\n-attack\n".to_owned()
}

/// Where the wall scene stands: in front of the dojo's front wall (a world brush at y = -632),
/// looking at it level (yaw 70).
const WALL_SPOT: [f32; 3] = [100.0, -760.0, 60.0];

/// The wall scene: with `ts_dev_fxlog 1` the client logs each predicted shot's first-person flash,
/// bullet, impact and decal (`[ts fx]` lines); three single shots at the wall, then a screenshot.
fn script_wall_shots() -> String {
    let [x, y, z] = WALL_SPOT;
    let mut cfg = format!("ts_dev_fxlog 1\nts_tp 1 {x} {y} {z} 70 0\n{}echo XTASK: wall\n", secs(1.0));
    for _ in 0..3 {
        cfg += &format!("+attack\nwait;wait\n-attack\n{}", secs(0.6));
    }
    cfg += &shot("wall shots");
    cfg + "echo XTASK: wall done\nts_dev_fxlog 0\n"
}

/// The numbers in a `[x, y, z]` list starting at `key`.
fn vec_after(line: &str, key: &str) -> Option<[f32; 3]> {
    let rest = &line[line.find(key)? + key.len()..];
    let inner = rest.strip_prefix('[')?.split(']').next()?;
    let v: Vec<f32> = inner.split(',').filter_map(|n| n.trim().parse().ok()).collect();
    Some([*v.first()?, *v.get(1)?, *v.get(2)?])
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Checks for the wall scene, from the `[ts fx]` lines between its markers. The first-person flash
/// must sit in front of the view near the view model's muzzle and spread along the view direction
/// (the predicted events used to play with zero origin and angles, so the flash streaked along
/// world +x and the bullets flew from the world origin), and the bullets must hit the wall in front
/// of the shooter with impacts and decals.
fn wall_checks(text: &str) -> Vec<(&'static str, bool)> {
    // `echo` ends its line with a space.
    let mut fx: Vec<&str> = Vec::new();
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim_end();
        if t.ends_with("XTASK: wall done") {
            break;
        }
        if t.ends_with("XTASK: wall") {
            inside = true;
        } else if let Some((_, r)) = l.split_once("[ts fx] ").filter(|_| inside) {
            fx.push(r);
        }
    }
    let num = |l: &str, key: &str| field(l, key).and_then(|v| v.parse::<f32>().ok());
    let flashes: Vec<&str> = fx.iter().copied().filter(|l| l.starts_with("flash at")).collect();
    let flash_ok = |l: &str| -> bool {
        let (Some(ahead), Some(off), Some(muzzle), Some(dir)) =
            (num(l, "ahead "), num(l, "off-axis "), num(l, "muzzle-distance "), num(l, "direction-dot "))
        else {
            return false;
        };
        ahead > 8.0 && off < ahead && muzzle < 8.0 && dir > 0.95
    };
    let near = |p: Option<[f32; 3]>, r: f32| p.is_some_and(|p| dist(p, WALL_SPOT) < r);
    let bullets: Vec<&str> = fx.iter().copied().filter(|l| l.starts_with("bullet from")).collect();
    let impacts = fx.iter().filter(|l| l.starts_with("impact on")).count();
    let decals: Vec<&str> = fx.iter().copied().filter(|l| l.starts_with("decal ")).collect();
    say!(
        "   wall scene: {} flashes, {} bullets, {impacts} impacts, {} decals{}",
        flashes.len(),
        bullets.len(),
        decals.len(),
        flashes.first().map_or(String::new(), |f| format!("; first flash: {f}"))
    );
    vec![
        (
            "wall: the first-person flash is at the gun's muzzle, in front of the view, along the view direction",
            !flashes.is_empty() && flashes.iter().all(|l| flash_ok(l)),
        ),
        (
            "wall: the bullets fly from the shooter's eye and hit the wall in front",
            bullets.len() >= 3 && bullets.iter().all(|l| near(vec_after(l, "from "), 64.0) && near(vec_after(l, " to "), 400.0)),
        ),
        (
            "wall: bullet impacts and decals on the wall",
            impacts >= 3 && decals.len() >= 3 && decals.iter().all(|l| near(vec_after(l, " at "), 400.0)),
        ),
    ]
}

/// The `[ts fx]` lines between `XTASK: <tag>` and `XTASK: <tag> done` (`echo` ends its line with a
/// space).
fn fx_lines<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let (start, end) = (format!("XTASK: {tag}"), format!("XTASK: {tag} done"));
    let mut out = Vec::new();
    let mut inside = false;
    for l in text.lines() {
        let t = l.trim_end();
        if t.ends_with(&end) {
            break;
        }
        if t.ends_with(&start) {
            inside = true;
        } else if let Some((_, r)) = l.split_once("[ts fx] ").filter(|_| inside) {
            out.push(r.trim_end());
        }
    }
    out
}

/// A stopped bot for the laser scene (`play` step 2, shared with `steam`). It joins as an observer
/// and respawns after the 5 s wait, with akimbo Berettas and a laser. Its own random loadout can
/// weigh too much for the Berettas (TS's `PickUpWeapon` refuses a gun the free slots can't take, so
/// a `ts_give` then did nothing and the bot had no beam, issue #179): it gets them as its loadout
/// instead, the buy menu's commands (`tkw <id>_<attachments>`, the second gun bare), while it waits
/// to respawn. That replaces what its buy module chose, or stops it from buying (a bot buys only
/// with an empty loadout).
fn script_stopped_laser_bot() -> String {
    format!("ts_bot_stop 1\nts_bot_quota 1\n{}ts_bot_quota\nts_as 2 rst_cfg\nts_as 2 tkw 2_2 2_0\n{}", secs(1.0), secs(8.0))
}

/// The laser and akimbo scene, with the stopped bot #2 in the game, which spawned with akimbo
/// Berettas and a laser ([`Ctx::play`] step 2): the host gets the same (two `ts_give 2_2`; the
/// laser comes on with the gun), switches to free semi (`firemode`) and looks at the bot side on
/// from 200 units. Then two akimbo shots: both triggers in one frame (a linked double shot), and
/// the triggers 0.3 s apart (one shot per gun).
fn script_lasers_and_akimbo() -> String {
    let mut cfg = "ts_dev_fxlog 1\nts_give 2_2\nwait;wait\nts_give 2_2\n".to_owned();
    cfg += &secs(1.0);
    cfg += "firemode\n";
    cfg += &format!("ts_tp 2 0 -480 60 270 0\n{}ts_tp 1 200 -480 60 180 0\n{}ts_face 1 2\n", secs(0.5), secs(0.5));
    cfg += &format!("{}echo XTASK: lasers\n{}", secs(0.5), secs(2.0));
    cfg += &shot("lasers");
    // The shots go past the bot, which the later scenes need alive.
    cfg += &format!("echo XTASK: lasers done\nts_face 1 200 -1000 60\n{}", secs(0.5));
    cfg += "echo XTASK: akimbo\n+attack\n+attack2\nwait;wait\n-attack\n-attack2\n";
    cfg += &secs(1.0);
    cfg += &format!("+attack\n{}+attack2\nwait;wait\n-attack\n-attack2\n{}", secs(0.3), secs(1.0));
    cfg + "echo XTASK: akimbo done\nts_dev_fxlog 0\nweapon_13\n"
}

/// Checks for the laser scene, from the `[ts fx] laser` lines. First person: two local beams
/// (akimbo), each starting in front of the eye at its gun's view-model muzzle (the right gun to the
/// right of the left one). The bot's beam (TS draws one per other player, akimbo or not) starts at
/// its `p_` model's gun hand, away from the entity origin (the attachments come from the drawn
/// model, not the origin FWGS resets them to), near the bot's eye.
fn laser_checks(text: &str) -> Vec<(&'static str, bool)> {
    let fx = fx_lines(text, "lasers");
    let local = |gun: &str| -> Vec<&str> { fx.iter().copied().filter(|l| l.starts_with(&format!("laser local gun {gun} "))).collect() };
    let (g0, g1) = (local("0"), local("1"));
    let others: Vec<&str> = fx.iter().copied().filter(|l| l.starts_with("laser other #2 ")).collect();
    let num = |l: &str, key: &str| field(l, key).and_then(|v| v.parse::<f32>().ok());
    let local_ok = |l: &str| num(l, "ahead ").is_some_and(|a| a > 0.0 && a < 48.0) && num(l, "right ").is_some_and(|r| r.abs() < 32.0);
    let right = |ls: &[&str]| ls.first().and_then(|l| num(l, "right "));
    let other_ok = |l: &str| {
        let (Some(beam), Some(hand), Some(origin), Some(eye)) =
            (vec_after(l, "beam "), vec_after(l, "hand "), vec_after(l, "origin "), vec_after(l, "eye "))
        else {
            return false;
        };
        dist(hand, origin) > 4.0 && dist(hand, origin) < 64.0 && dist(beam, hand) < 17.0 && dist(beam, eye) < 64.0
    };
    say!(
        "   laser scene: {} + {} local beam lines, {} for the bot{}",
        g0.len(),
        g1.len(),
        others.len(),
        others.first().map_or(String::new(), |l| format!("; first: {l}"))
    );
    vec![
        (
            "lasers: two first-person beams (akimbo), each from its gun's muzzle in front of the eye",
            !g0.is_empty() && !g1.is_empty() && g0.iter().chain(&g1).all(|l| local_ok(l)) && right(&g0) > right(&g1),
        ),
        ("lasers: the bot's beam starts at its gun hand, not at its origin", !others.is_empty() && others.iter().all(|l| other_ok(l))),
    ]
}

/// Checks for the akimbo shots: both triggers in one frame fire both guns (a linked shot); the
/// triggers apart fire one gun each (free semi: `+attack` the `b0` gun, `+attack2` the `b1` gun).
fn akimbo_checks(text: &str) -> Vec<(&'static str, bool)> {
    let shots: Vec<&str> = fx_lines(text, "akimbo").into_iter().filter(|l| l.starts_with("akimbo shot #1 ")).collect();
    say!("   akimbo shots: {shots:?}");
    let kinds: Vec<&str> = shots.iter().map(|l| l.rsplit(' ').next().unwrap_or("")).collect();
    vec![("akimbo free semi: both triggers fire both guns (linked), then each trigger its own gun", kinds == ["linked", "b0", "b1"])]
}

/// The gameplay ruleset and a reload (issues #100, #104, `docs/implementation/gameplay-plugins.md`
/// §4), with the four quota bots playing on `map`. `ts_gameplay 21` and straight back to 30 in one
/// line, and `ts_bot_backend NATIVE` (the running backend under another spelling), change nothing
/// the map runs: no reload, the scores are kept. `ts_gameplay 21` alone reloads the map under 2.1
/// (the server's 2.1 rules, physinfo `gp` 21, the client's `SrvSett` 4) without a `changelevel`,
/// once, and the bots play it; `ts_gameplay 30` reloads it under 3.0, and the host joins through
/// the buy menu as at the start.
fn script_gameplay_switch() -> String {
    let mut s = status("before switch");
    s += &format!("echo XTASK: gameplay set and back\nts_gameplay 21;ts_gameplay 30\nts_bot_backend NATIVE\n{}", secs(1.5));
    s += &status("kept");
    s += &format!("ts_bot_backend native\necho XTASK: gameplay 21\nts_gameplay 21\n{}", secs(15.0));
    s += &status("map 21");
    s += "echo XTASK: gameplay 30\nts_gameplay 30\n";
    s += &script_join_and_spawn();
    s + &status("map 30")
}

fn gameplay_switch_checks(text: &str, dumps: &[(String, Vec<PlayerStatus>)], map: &str) -> Vec<(&'static str, bool)> {
    let dump = |tag: &str| dumps.iter().find(|d| d.0 == tag).map(|d| d.1.clone()).unwrap_or_default();
    let after = |marker: &str| text.split_once(marker).map_or("", |(_, a)| a);
    // Each part of the log, up to the next scene marker.
    let part = |from: &str, to: &str| after(from).split_once(to).map_or(after(from), |(a, _)| a);
    let mid_map = part("XTASK: gameplay set and back", "XTASK: gameplay 21");
    let map21 = part("XTASK: gameplay 21", "XTASK: gameplay 30");
    let map30 = after("XTASK: gameplay 30");
    let gp = |tag: &str, id: &str| {
        let d: Vec<PlayerStatus> = dump(tag).into_iter().filter(|p| p.state == "alive").collect();
        !d.is_empty() && d.iter().all(|p| p.gp == id)
    };
    let (before, kept_dump) = (dump("before switch"), dump("kept"));
    let kept = before.iter().any(|p| p.frags != 0 || p.deaths != 0)
        && before.iter().all(|p| kept_dump.iter().any(|q| q.idx == p.idx && q.frags >= p.frags && q.deaths >= p.deaths));
    // Bots playing the 2.1 map: alive at the dump, or already scored or died on it (the reload
    // zeroes the scores). One snapshot alone missed bots waiting to respawn (2 of 4 on fwgs-win).
    let bots21 = dump("map 21").iter().filter(|p| p.idx != 1 && (p.state == "alive" || p.frags != 0 || p.deaths != 0)).count();
    let reloads = |part: &str| part.matches(": reloading ").count();
    say!("   gameplay reload: scores kept with no change {kept}; bots playing the 2.1 map {bots21}");
    vec![
        (
            "ts_gameplay 21 and back at once, ts_bot_backend NATIVE: no reload, the map keeps running 3.0 (gp 30, scores kept)",
            reloads(mid_map) == 0 && !mid_map.contains("[ts] gameplay: ruleset ") && gp("kept", "30") && kept,
        ),
        (
            "ts_gameplay 21: one reload of the map under 2.1 (2.1's rules, physinfo gp 21, the client's SrvSett 4), the bots play it",
            map21.contains(&format!("[ts] gameplay: ruleset 21: reloading {map}"))
                && reloads(map21) == 1
                && map21.contains("[ts] gameplay: ruleset 21 for this map")
                && map21.contains("[ts] game rules (2.1): ")
                && map21.contains("[ts] gameplay: the server runs ruleset 21")
                && gp("map 21", "21")
                && bots21 >= 3,
        ),
        (
            "ts_gameplay 30: one reload under 3.0, and the host joined and spawned through the buy menu",
            map30.contains(&format!("[ts] gameplay: ruleset 30: reloading {map}"))
                && reloads(map30) == 1
                && map30.contains("[ts] gameplay: ruleset 30 for this map")
                && map30.contains("[ts] game rules: ")
                && map30.contains("[ts] gameplay: the server runs ruleset 30")
                && gp("map 30", "30")
                && dump("map 30").iter().any(|p| p.idx == 1 && p.state == "alive"),
        ),
    ]
}

/// The `addbot` scene (after the quota bots are gone): a listen server adds a bot named from
/// `bots.txt`, as TS's Windows server does, and the quota (0) leaves it alone.
fn script_addbot() -> String {
    format!("echo XTASK: addbot\naddbot\n{}{}", secs(3.0), status("addbot"))
}

fn addbot_checks(text: &str, dumps: &[(String, Vec<PlayerStatus>)]) -> Vec<(&'static str, bool)> {
    let bot = dumps.iter().find(|d| d.0 == "addbot").and_then(|d| d.1.iter().find(|p| p.idx != 1 && !p.name.starts_with("Bot ")).cloned());
    let after = text.split_once("XTASK: addbot").map_or("", |(_, a)| a);
    let named = bot.as_ref().is_some_and(|b| after.contains(&format!("{} connected with skill", b.name)));
    say!("   addbot: {:?}", bot.as_ref().map(|b| (&b.name, &b.state)));
    vec![(
        "addbot on a listen server added a bot (bots.txt name), and the quota kept it",
        named && !after.contains("Bots only in listen servers") && bot.is_some_and(|b| !after.contains(&format!("{} was kicked", b.name))),
    )]
}

/// Node paths `testpath` prints on ts_dojo, start first. The original TS 3.0 (Windows `mp.dll`
/// under FWGS win32) prints all three. The decomp's `scripts/ndm.py` replica of `FindPath` gives
/// the first two; for 89 → 25 it rounds the walk's heuristic to a float, which mp.dll's x87 code
/// doesn't, and gets `89,87,25` (the bots package's unit tests compare every pair against the
/// replica with that rounding removed).
const TESTPATHS: [(i32, i32, &str); 3] = [(0, 50, "path:0,47,50 "), (10, 100, "path:10,15,63,84,100 "), (89, 25, "path:89,88,25 ")];

/// TS's bot AI with `addbot` bots on ts_dojo, on its own listen server (the play scene's quota
/// kicks wait behind its script, and `maxplayers 8` leaves no room). The host joins, spawns and
/// suicides, so it watches as an observer:
/// 1. `testpath` on the shipped ts_dojo graph with `botlearn 2`, which prints the path
///    ([`TESTPATHS`]).
/// 2. Four `addbot`s play for 40 s. `botlearn 2` makes TS print every node path the bots plan
///    (`(debug)arrived` + `path:...`), so the roaming is seen to use the graph.
/// 3. `resetnodes`: with `botlearn 2` the bots' own walking learns a new graph (`(debug)N nodes`).
///    Then `loadnodes` puts the shipped graph back and `botlearn 0`. (`savenodes` isn't run: the
///    roots link the content's `maps` directory.)
fn script_ts_bots() -> String {
    let mut s = format!("kill\n{}echo XTASK: testpath\nbotlearn 2\n", secs(2.0));
    for (a, b, _) in TESTPATHS {
        s += &format!("testpath {a} {b}\n");
    }
    s += &secs(2.0);
    s += "echo XTASK: tsbots\naddbot\naddbot\naddbot\naddbot\n";
    for i in 0..8 {
        s += &secs(5.0);
        s += &status(&format!("tsbots {i}"));
    }
    s += &format!("echo XTASK: learn\nresetnodes\n{}echo XTASK: learned\nloadnodes\nbotlearn 0\n", secs(6.0));
    s
}

fn ts_bots_checks(text: &str, dumps: &[(String, Vec<PlayerStatus>)], host_name: &str) -> Vec<(&'static str, bool)> {
    let section = |from: &str, to: &str| text.split_once(from).map_or("", |(_, a)| a.split_once(to).map_or(a, |(b, _)| b)).to_owned();
    // The client may print the replies after the bots have joined (and planned paths of their
    // own), so the whole scene is searched for them.
    let testpath = section("XTASK: testpath", "XTASK: learn");
    let paths: Vec<&str> = testpath.lines().filter_map(|l| l.find("path:").map(|i| &l[i..])).collect();
    say!("   testpath: {:?}", TESTPATHS.iter().map(|t| (t.2, paths.contains(&t.2))).collect::<Vec<_>>());
    let tsbots = section("XTASK: tsbots", "XTASK: learn");
    let planned = tsbots.lines().filter(|l| l.contains("(debug)arrived")).count();
    // Movement of the TS bots (bots.txt names) within a life, as for the quota bots.
    let bot_dumps: Vec<&Vec<PlayerStatus>> = dumps.iter().filter(|d| d.0.starts_with("tsbots ")).map(|d| &d.1).collect();
    let mut moved: std::collections::BTreeMap<String, f32> = Default::default();
    for w in bot_dumps.windows(2) {
        for a in w[0].iter().filter(|p| p.idx != 1 && p.state == "alive") {
            if let Some(b) = w[1].iter().find(|b| b.idx == a.idx && b.name == a.name && b.deaths == a.deaths && b.state == "alive") {
                let d = ((a.org[0] - b.org[0]).powi(2) + (a.org[1] - b.org[1]).powi(2)).sqrt();
                let m = moved.entry(a.name.clone()).or_default();
                *m = m.max(d);
            }
        }
    }
    let moving = moved.values().filter(|&&d| d > 64.0).count();
    let names: Vec<String> =
        bot_dumps.last().map(|d| d.iter().filter(|p| p.idx != 1).map(|p| p.name.clone()).collect()).unwrap_or_default();
    let kills = tsbots
        .lines()
        .filter(|l| l.contains(" killed ") && !l.contains(host_name) && names.iter().any(|n| l.contains(&format!("{n} killed "))))
        .count();
    let learn = section("XTASK: learn", "XTASK: learned");
    let learned = (1..=3).all(|n| learn.contains(&format!("(debug){n} nodes")));
    say!("   TS bots: {} planned node paths, largest moves {moved:?}, kills {kills}; learning after resetnodes: {learned}", planned);
    vec![
        ("testpath prints the node paths of scripts/ndm.py's FindPath replica", TESTPATHS.iter().all(|t| paths.contains(&t.2))),
        (
            "four addbot bots roam over the node graph (planned paths), move and fight",
            names.len() >= 4 && planned >= 2 && moving >= 2 && moving + 1 >= moved.len() && kills >= 1,
        ),
        ("botlearn 2 learns nodes from the bots' walk after resetnodes", learned),
    ]
}

/// The Escape scene, last in a script: `cancelselect` is what Escape runs in game on Steam GoldSrc
/// and FWGS 0.21. With no weapon menu open it sends `escape` (the engine's main menu); with one
/// open it closes it first, so the scene runs it twice.
fn script_escape() -> String {
    "ts_dev_keylog 1\necho XTASK: escape\ncancelselect\nwait;wait\ncancelselect\nwait;wait\n".to_owned()
}

fn escape_checks(text: &str) -> Vec<(&'static str, bool)> {
    let after = text.split_once("XTASK: escape").map_or("", |(_, a)| a);
    vec![(
        "Escape (cancelselect) with no menu open sends escape to the engine's main menu",
        after.contains("[ts key] cancelselect: escape"),
    )]
}

/// `ts_status` dump tagged so checks can find it.
fn status(tag: &str) -> String {
    format!("echo XTASK: status {tag}\nts_status\n")
}

/// One player line of a `ts_status` dump.
#[derive(Debug, Clone)]
struct PlayerStatus {
    idx: i32,
    name: String,
    state: String,
    frags: i32,
    deaths: i32,
    held: String,
    active: String,
    slow_target: f32,
    /// Physinfo `gp`: the gameplay ruleset the player moves by (empty before its first spawn).
    gp: String,
    org: [f32; 3],
}

/// The commands that start game clients or Wine servers by default (`server` runs one chosen
/// root, often a Linux one that needs no display; `relaunch` checks for itself: `--xvfb`).
fn launches_games(cmd: &str) -> bool {
    matches!(
        cmd,
        "test" | "meck" | "bots" | "plugins" | "client-plugins" | "ts21" | "version" | "tspkg" | "play" | "all" | "client" | "steam"
    )
}

/// Without `DISPLAY` or `WAYLAND_DISPLAY` (an agent's shell, ssh), FWGS runs as a dedicated server
/// (`SDL_Init failed`, `Unknown command "connect"`) and Wine's `hlds.exe` dies reading its console
/// (`CTextConsoleWin32::GetLine`), so every client check fails late and looks like a code bug
/// (docs/validation.md, troubleshooting). Stop at once instead.
fn require_display() {
    if cfg!(target_os = "linux") && !has_display(env::var_os("DISPLAY"), env::var_os("WAYLAND_DISPLAY")) {
        eprintln!(
            "xtask: no DISPLAY or WAYLAND_DISPLAY: the games need a display. Start an Xvfb \
             (`Xvfb :87 -screen 0 1280x800x24`) and run with `DISPLAY=:87 SDL_VIDEODRIVER=x11` \
             (docs/validation.md)."
        );
        exit(2);
    }
}

fn has_display(x11: Option<std::ffi::OsString>, wayland: Option<std::ffi::OsString>) -> bool {
    [x11, wayland].into_iter().flatten().any(|v| !v.is_empty())
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = &line[line.find(key)? + key.len()..];
    Some(rest.split_whitespace().next().unwrap_or(""))
}

fn parse_status_line(line: &str) -> Option<PlayerStatus> {
    let rest = line.split("[ts]  #").nth(1)?;
    let (idx, rest) = rest.split_once(' ')?;
    let state_at = [" alive ", " dead ", " observer "].iter().filter_map(|s| rest.find(s).map(|i| (i, *s))).min()?;
    let name = rest[..state_at.0].trim().to_owned();
    let org = line.split("org=(").nth(1)?.trim_end_matches(')');
    let o: Vec<f32> = org.split_whitespace().filter_map(|v| v.trim_end_matches(')').parse().ok()).collect();
    Some(PlayerStatus {
        idx: idx.parse().ok()?,
        name,
        state: state_at.1.trim().to_owned(),
        frags: field(line, "frags=")?.parse().ok()?,
        deaths: field(line, "deaths=")?.parse().ok()?,
        held: field(line, "held=")?.split('/').next()?.to_owned(),
        active: field(line, "active=")?.to_owned(),
        slow_target: field(line, "slow=")?.split("->").nth(1)?.parse().ok()?,
        gp: field(line, "gp=").unwrap_or("").to_owned(),
        org: [*o.first()?, *o.get(1)?, *o.get(2)?],
    })
}

/// The `ts_status` dumps of a run, in order: (tag, players).
fn status_dumps(text: &str) -> Vec<(String, Vec<PlayerStatus>)> {
    let mut out: Vec<(String, Vec<PlayerStatus>)> = Vec::new();
    for line in text.lines() {
        if let Some(tag) = line.split("XTASK: status ").nth(1) {
            out.push((tag.trim().to_owned(), Vec::new()));
        } else if let (Some(last), Some(p)) = (out.last_mut(), parse_status_line(line)) {
            last.1.push(p);
        }
    }
    out
}

struct Run {
    text: String,
    shots: Vec<PathBuf>,
    /// The game exited on its own (a crash: scripts never `quit`).
    exited: bool,
    /// The script reached its last line (`ts_dev_touch` marker).
    done: bool,
}

impl Ctx {
    /// Runs the FWGS client of `root` as a listen server on `port` with `cfg`, and stops it
    /// `linger` after the script's last line. The script ends with `ts_dev_touch`, which creates a
    /// marker file at once (the engine log can sit in a write buffer until exit under Wine).
    /// Scripts don't `quit`: server commands the game library queues (the bot quota's `kick`) wait
    /// behind the script in the console buffer, so they only run once it has ended.
    fn run_script(&self, root: &Root, tag: &str, cfg: &str, port: u16, linger: Duration, limit: Duration) -> Run {
        self.run_script_env(root, tag, cfg, port, linger, limit, &[])
    }

    /// [`Self::run_script`] with extra environment variables for the game.
    #[allow(clippy::too_many_arguments)] // run_script's, plus the environment
    fn run_script_env(&self, root: &Root, tag: &str, cfg: &str, port: u16, linger: Duration, limit: Duration, env: &[(&str, &str)]) -> Run {
        let gamedir = root.path.join("ts");
        let cfg_name = format!("xtask_{tag}.cfg");
        let marker = gamedir.join(format!("xtask_{tag}_done"));
        let _ = fs::remove_file(&marker);
        fs::write(gamedir.join(&cfg_name), format!("{cfg}echo XTASK: done\nts_dev_touch xtask_{tag}_done\n")).unwrap();
        let _ = fs::remove_dir_all(gamedir.join("scrshots"));
        let log_path = self.repo.join("target").join(format!("{tag}-{}.log", root.name));
        let log = fs::File::create(&log_path).unwrap();
        let (port, clientport) = (port.to_string(), (port + 1).to_string());
        let args = [
            "-game",
            "ts",
            "-dev",
            "2",
            "-log",
            "-windowed",
            "-width",
            "1024",
            "-height",
            "768",
            "-port",
            &port,
            "-clientport",
            &clientport,
            "+exec",
            &cfg_name,
        ];
        let mut cmd = if root.os == Os::Windows {
            let mut c = Command::new("wine");
            c.env("WINEPREFIX", roots::wineprefix()).env("WINEDEBUG", "-all").arg("xash3d.exe");
            c
        } else {
            Command::new(root.path.join("xash3d"))
        };
        let _ = fs::remove_file(root.log_path());
        cmd.envs(env.iter().copied());
        let mut child = cmd.current_dir(&root.path).args(args).stdout(log.try_clone().unwrap()).stderr(log).spawn().expect("start client");
        let t = Instant::now();
        let (mut exited, mut done_at) = (false, None);
        while t.elapsed() < limit {
            if child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            if done_at.is_none() && marker.exists() {
                done_at = Some(Instant::now());
            }
            if done_at.is_some_and(|d| d.elapsed() >= linger) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = child.kill();
        let _ = child.wait();
        root.cleanup();
        say!("   script ran {:.0}s ({}x game time)", done_at.map_or(t.elapsed(), |d| d - t).as_secs_f32(), TIMESCALE);

        // engine.log (-log) has everything on both builds; stdout is the fallback. Not both:
        // the checks count lines.
        let engine_log = String::from_utf8_lossy(&fs::read(root.log_path()).unwrap_or_default()).into_owned();
        let text = if engine_log.contains("XTASK:") {
            engine_log
        } else {
            String::from_utf8_lossy(&fs::read(&log_path).unwrap_or_default()).into_owned()
        };
        let text = text.replace('\r', "");
        let mut shots: Vec<PathBuf> =
            fs::read_dir(gamedir.join("scrshots")).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        shots.sort();
        let shots = shots
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let dest = self.repo.join("target").join(format!("{tag}-{}-{}.png", root.name, i + 1));
                let _ = fs::copy(p, &dest);
                dest
            })
            .collect();
        Run { text, shots, exited, done: done_at.is_some() }
    }

    fn report(&self, root: &Root, tag: &str, run: &Run, checks: &[(&str, bool)]) -> bool {
        for p in &run.shots {
            say!("   screenshot: {}", p.display());
        }
        let mut ok = true;
        for (what, pass) in checks {
            say!("   {} {what}", if *pass { "ok  " } else { "FAIL" });
            ok &= *pass;
        }
        let log = self.repo.join("target").join(format!("{tag}-{}.log", root.name));
        say!("   {} (full log: {}; engine log: {})", if ok { "PASS" } else { "FAIL" }, log.display(), root.log_path().display());
        ok
    }

    /// The gameplay check: one listen server on ts_dojo, played through every path the
    /// dedicated-server checks can't reach.
    /// 1. Join: the buy menu opens; reset its random first loadout, buy an AK47 with a scope
    ///    (`slotN`, as the number keys do), click `+attack` after the respawn wait to spawn. Then
    ///    fire three shots at a wall ([`wall_checks`]: flash, bullets, impacts, decals).
    /// 2. `ts_bot_quota 1` (with `ts_bot_stop 1`, so the bot holds still) adds a bot. Then the
    ///    laser and akimbo scene ([`script_lasers_and_akimbo`]).
    /// 3. With the bot in view, step onto the slow-motion power-up (`0 -672 48`, pwuptype 1) and
    ///    double-tap `+use`: the host runs at 0.6 and slows the bot to 0.35.
    /// 4. Take an M61, aim at a 1-HP wooden pillar (`*15`) and throw: the explosion breaks it.
    /// 5. Put the bot in front of the host, who shoots it dead.
    /// 6. Kick the dead bot. A dropped bot used to stay in the bot list and crash the engine in
    ///    `pfnSetClientMaxspeed` when it "respawned"; the quota then adds a replacement.
    /// 7. The host is killed (`ts_hurt`), clicks through the death cam and clicks to respawn.
    /// 8. The host suicides and stays observer; four bots (`ts_bot_stop 0`) play for 70 s: they
    ///    move around and kill each other. Then `ts_bot_quota 0` kicks them.
    /// 9. `addbot` adds a TS listen-server bot the quota keeps; then Escape (`cancelselect`) sends
    ///    `escape`.
    fn play(&self, root: &Root, port: u16) -> bool {
        say!("== {}: gameplay on ts_dojo (listen server, port {port})", root.name);
        let mut cfg = script_start("ts_dojo") + &script_join_and_spawn() + &status("spawned");

        // 1b. shoot the dojo's front wall: flash, bullets, impacts and decals
        cfg += &script_wall_shots();

        // 2. a stopped bot with akimbo Berettas and a laser for the laser scene
        cfg += &script_stopped_laser_bot();

        // 2b. lasers (first person and on the bot) and akimbo free semi
        cfg += &script_lasers_and_akimbo();

        // 3. power-up with the bot in view
        cfg += &format!("ts_tp 1 0 -672 60 90 0\n{}ts_tp 2 0 -480 60 270 0\n{}", secs(0.5), secs(1.5));
        cfg += &status("picked up");
        // Frame counts stretch in game time when the client runs below `fps_max 100` (Xvfb's
        // software GL: about 45 fps native, 30-40 under Wine), issue #325. So the two presses are
        // two frames apart, well inside the client's 0.3 s double-tap window (six frames were
        // 0.3 s under Wine and the second press only re-armed it), and the status comes 0.5 s
        // later, inside the 3 s power-up (at 1.5 s it had run out below about 55 fps). The rest
        // of the old 4.5 s waits after the screenshot.
        cfg += "+use\nwait\n-use\nwait\n+use\nwait\n-use\n";
        cfg += &secs(0.5);
        cfg += &status("power-up used");
        cfg += &shot("slow motion");
        cfg += &secs(4.0);

        // 4. grenade; the host leaves before it goes off
        cfg += &format!("ts_give 24\n{}weapon_24\n{}", secs(0.5), secs(1.5));
        cfg += &format!("ts_tp 1 -332 -420 60 90 0\n{}ts_face 1 -332 -164 60\n{}", secs(1.0), secs(0.5));
        cfg += "echo XTASK: throw\n+attack\nw10\n-attack\n";
        cfg += &secs(1.0);
        cfg += &format!("ts_tp 1 0 -1344 60 90 0\n{}", secs(5.0));

        // 5. the host shoots the stopped bot
        cfg += &format!("weapon_13\n{}ts_tp 2 0 -1200 60 270 0\n{}", secs(1.5), secs(0.5));
        cfg += "echo XTASK: shoot\n+attack\n";
        // 5 s: recoil climbs and each hit knocks the bot back a little, so 3 s sometimes left it
        // alive on fwgs-win.
        for _ in 0..25 {
            cfg += &format!("ts_face 1 2\n{}", secs(0.2));
        }
        cfg += "-attack\n";
        cfg += &status("bot shot");

        // 6. kick the dead bot; the quota (1) adds another
        cfg += &format!("kick \"Bot Neo\"\n{}", secs(4.0));

        // 7. the host dies and clicks to respawn
        cfg += "echo XTASK: host killed\nts_hurt 1 500\n";
        cfg += &secs(2.0);
        cfg += &click();
        cfg += &secs(1.0);
        cfg += &status("host dead");
        cfg += &secs(5.5);
        cfg += &click();
        cfg += &secs(2.0);
        cfg += &status("host respawned");
        cfg += &shot("respawned");

        // 8. bots on their own; dumps 5 s apart because movement is only measured within a life
        cfg += &format!("kill\nts_bot_stop 0\n{}ts_bot_quota 4\n", secs(3.0));
        for i in 0..14 {
            cfg += &secs(5.0);
            cfg += &status(&format!("bots {i}"));
        }
        cfg += &shot("bots");

        // 8b. the gameplay ruleset: setting ts_gameplay reloads the map, under 2.1 and then 3.0
        // again (the bots play on, the host joins again)
        cfg += &script_gameplay_switch();
        cfg += "ts_bot_quota 0\n";

        // 9. `addbot` (TS's listen-server bots), then Escape last: its `escape` opens the menu
        cfg += &script_addbot();
        cfg += &script_escape();

        // Linger for the quota's kicks (one per 0.5 s game time, queued behind the script).
        let run = self.run_script(root, "play", &cfg, port, Duration::from_secs(5), Duration::from_secs(420));
        let text = &run.text;
        for line in text.lines().filter(|l| {
            l.contains("XTASK:") && !l.contains("XTASK: status")
                || l.contains("spawned (#1)")
                || l.contains("func_breakable") && !l.contains("by Bot")
                || l.contains("kicked")
        }) {
            say!("   | {}", line.trim_end());
        }

        let dumps = status_dumps(text);
        let dump = |tag: &str| dumps.iter().find(|d| d.0 == tag).map(|d| d.1.clone()).unwrap_or_default();
        let host = |tag: &str| dump(tag).into_iter().find(|p| p.idx == 1);
        let host_name = host("spawned").map(|p| p.name).unwrap_or_default();
        let spawns: Vec<&str> = text.lines().filter(|l| l.contains("[ts] ") && l.contains("spawned (#1)")).collect();
        let kick_at = text.find("Bot Neo was kicked");
        let after_kick = |needle: &str| kick_at.is_some_and(|k| text[k..].contains(needle));

        // Bots: positions in consecutive dumps of the same life, and bot-on-bot kills.
        let bot_dumps: Vec<&Vec<PlayerStatus>> = dumps.iter().filter(|d| d.0.starts_with("bots ")).map(|d| &d.1).collect();
        let mut moved: std::collections::BTreeMap<String, f32> = Default::default();
        for w in bot_dumps.windows(2) {
            for a in w[0].iter().filter(|p| p.name.starts_with("Bot ") && p.state == "alive") {
                if let Some(b) = w[1].iter().find(|b| b.idx == a.idx && b.name == a.name && b.deaths == a.deaths && b.state == "alive") {
                    let d = ((a.org[0] - b.org[0]).powi(2) + (a.org[1] - b.org[1]).powi(2)).sqrt();
                    let m = moved.entry(a.name.clone()).or_default();
                    *m = m.max(d);
                }
            }
        }
        let moving = moved.values().filter(|&&d| d > 64.0).count();
        let bot_kills = text.lines().filter(|l| l.contains("Bot ") && l.contains(" killed Bot ")).count();
        say!("   bots' largest move within a life: {moved:?}; bot-on-bot kills: {bot_kills}");
        let last_bots = bot_dumps.last().map(|d| d.iter().filter(|p| p.name.starts_with("Bot ")).count()).unwrap_or(0);

        let spawn_line = spawns.first().copied().unwrap_or("");
        let mut checks = vec![
            ("join: bought an AK47 and clicked to spawn", spawn_line.split_once(" with ").is_some_and(|(_, w)| w.contains("13"))),
            ("ts_bot_quota 1 added a bot", text.contains("\"ts_bot_quota\" is \"1\"") && text.contains("[ts] bots: added")),
            ("picked up the slow-motion power-up", host("picked up").is_some_and(|p| p.held == "0x1")),
            ("+use double tap: host at 0.6, the bot in view at 0.35", {
                let d = dump("power-up used");
                let host_ok = d.iter().any(|p| p.idx == 1 && p.active == "0x1" && (p.slow_target - 0.6).abs() < 0.01);
                host_ok && d.iter().any(|p| p.idx != 1 && (p.slow_target - 0.35).abs() < 0.01)
            }),
            ("the thrown grenade broke a func_breakable", text.contains(&format!("broken by {host_name} (grenade"))),
            (
                "the host shot a bot dead",
                text.contains(&format!("{host_name} killed Bot ")) && host("bot shot").is_some_and(|p| p.frags >= 1),
            ),
            ("kicking the dead bot: no crash, the quota replaced it", after_kick("[ts] bots: added") && !run.exited),
            ("the host died and clicked to respawn", {
                host("host dead").is_some_and(|p| p.deaths >= 1 && p.state != "alive")
                    && spawns.len() >= 2
                    && host("host respawned").is_some_and(|p| p.state == "alive")
            }),
            ("four bots moved around and killed each other", {
                last_bots >= 4 && moved.len() >= 2 && moving >= 2 && moving >= moved.len() - 1 && bot_kills >= 2
            }),
            ("ts_bot_quota 0 kicked the bots", text.matches("was kicked").count() >= 4),
            ("the script finished, no panic or crash", run.done && !run.exited && !crashed(text)),
        ];
        checks.splice(1..1, wall_checks(text));
        let at = checks.iter().position(|c| c.0 == "ts_bot_quota 1 added a bot").map_or(checks.len(), |i| i + 1);
        checks.splice(at..at, laser_checks(text).into_iter().chain(akimbo_checks(text)));
        let at = checks.len() - 1;
        checks.splice(
            at..at,
            gameplay_switch_checks(text, &dumps, "ts_dojo").into_iter().chain(addbot_checks(text, &dumps)).chain(escape_checks(text)),
        );
        self.report(root, "play", &run, &checks)
    }

    /// Grenades on a big custom map: a listen server on `MecklenburgV_Rain_Final` (a third-party
    /// map with 37 `func_breakable`s, rain emitters, fog and wind).
    /// 1. Join and spawn as in [`Ctx::play`]; add one stopped bot.
    /// 2. Stand between two 10-HP walls (`*113`, `*114`), take an M61 (110 damage), drop it at
    ///    the host's feet (thrown straight down) and leave; the stopped bot is moved onto the
    ///    grenade. The blast breaks both walls and hurts the bot (a player takes half of it).
    /// 3. The same between two 50-HP walls (`*217`, `*220`): they break and the bot dies.
    fn meck(&self, root: &Root, port: u16) -> bool {
        const MAP: &str = "MecklenburgV_Rain_Final";
        say!("== {}: grenades on {MAP} (listen server, port {port})", root.name);
        let mut cfg = script_start(MAP) + &script_join_and_spawn() + &status("spawned");
        cfg += &format!("ts_bot_stop 1\nts_bot_quota 1\n{}", secs(8.0));
        cfg += &status("bot added");

        // A far spawn point to wait at while each grenade goes off.
        let away = "ts_tp 1 1900 -2845 -250 90 0\n";
        // Stand at `at`, look straight down and throw: the grenade drops at the host's feet. The
        // host is gone before the fuse runs out.
        let drop_at = |at: [i32; 3], then: &str| {
            format!(
                "ts_give 24\n{}weapon_24\n{}ts_tp 1 {} {} {} 90 89\n{}echo XTASK: throw\n{}+attack\n{}-attack\n{}{away}{then}{}",
                secs(0.5),
                secs(3.0),
                at[0],
                at[1],
                at[2],
                secs(1.0),
                status("grenade in hand"),
                secs(0.5),
                secs(0.4),
                secs(5.0)
            )
        };

        // 2. between the two 10-HP walls; once the host has gone, the bot stands on the grenade
        cfg += &format!("ts_tp 2 -2290 -1300 -380 90 0\n{}", secs(1.0));
        cfg += &status("bot placed");
        cfg += &drop_at([-2290, -1150, -380], "ts_tp 2 -2290 -1150 -380 90 0\n");
        cfg += &status("after grenade 1");
        cfg += &shot("grenade 1");

        // 3. between the two 50-HP walls
        cfg += &drop_at([-300, 712, -340], "ts_tp 2 -300 712 -340 90 0\n");
        cfg += &status("after grenade 2");
        cfg += &shot("grenade 2");
        cfg += "ts_bot_quota 0\n";

        let run = self.run_script(root, "meck", &cfg, port, Duration::from_secs(3), Duration::from_secs(300));
        let text = &run.text;
        for line in text.lines().filter(|l| {
            l.contains("XTASK:") && !l.contains("XTASK: status")
                || l.contains("ts_tp:")
                || l.contains("func_breakable")
                || l.contains(" killed ")
                || l.contains("(port guard)")
        }) {
            say!("   | {}", line.trim_end());
        }
        let dumps = status_dumps(text);
        let host = |tag: &str| dumps.iter().find(|d| d.0 == tag).and_then(|d| d.1.iter().find(|p| p.idx == 1).cloned());
        let host_name = host("spawned").map(|p| p.name).unwrap_or_default();
        let broken_in = |part: &str| {
            part.lines().filter(|l| l.contains("func_breakable") && l.contains(&format!("broken by {host_name} (grenade"))).count()
        };
        let (first, second) = text.split_once("XTASK: grenade 1").unwrap_or((text, ""));
        let (broken1, broken2) = (broken_in(first), broken_in(second));
        let checks = [
            ("the map loaded and the host spawned", host("spawned").is_some_and(|p| p.state == "alive")),
            ("ts_bot_quota 1 added a bot", text.contains("[ts] bots: added")),
            (
                "a thrown grenade killed the bot",
                text.lines()
                    .any(|l| l.contains(host_name.as_str()) && l.contains(" killed Bot ") && l.to_ascii_lowercase().contains("grenade"))
                    && host("after grenade 2").is_some_and(|p| p.frags >= 1),
            ),
            ("each grenade broke func_breakables (at least three in all)", broken1 >= 1 && broken2 >= 1 && broken1 + broken2 >= 3),
            ("the host survived both grenades", host("after grenade 2").is_some_and(|p| p.state == "alive")),
            ("no port guard refused a call", !text.contains("(port guard)")),
            ("the script finished, no panic or crash", run.done && !run.exited && !crashed(text)),
        ];
        say!("   func_breakables broken by the host's grenades: {broken1} + {broken2}");
        self.report(root, "meck", &run, &checks)
    }

    /// TS's bot AI ([`script_ts_bots`]).
    fn ts_bots(&self, root: &Root, port: u16) -> bool {
        say!("== {}: TS bots on ts_dojo (listen server, port {port})", root.name);
        let cfg = script_start("ts_dojo") + &script_join_and_spawn() + &status("spawned") + &script_ts_bots();
        let run = self.run_script(root, "bots", &cfg, port, Duration::from_secs(3), Duration::from_secs(300));
        let text = &run.text;
        for line in text.lines().filter(|l| l.contains("XTASK:") && !l.contains("XTASK: status") || l.contains("connected with skill")) {
            say!("   | {}", line.trim_end());
        }
        let dumps = status_dumps(text);
        let host_name = dumps.iter().find(|d| d.0 == "spawned").and_then(|d| d.1.iter().find(|p| p.idx == 1)).map(|p| p.name.clone());
        let mut checks = vec![("the host joined", host_name.is_some())];
        checks.extend(ts_bots_checks(text, &dumps, &host_name.unwrap_or_default()));
        checks.push(("the script finished, no panic or crash", run.done && !run.exited && !crashed(text)));
        self.report(root, "bots", &run, &checks)
    }
}

fn crashed(text: &str) -> bool {
    ["PANIC", "Sys_Crash", "Segmentation", "FATAL ERROR"].iter().any(|m| text.contains(m))
}

/// Links TS content into `gamedir`. Directories and binary assets are symlinked; small text
/// files the engine may rewrite (configs) are copied, so the source install is never modified.
fn link_ts_content(ts: &Path, gamedir: &Path) {
    const DIRS: &[&str] = &["events", "gfx", "logos", "maps", "models", "overviews", "resource", "sound", "sprites"];
    const LINK_EXT: &[&str] = &["wad", "tga", "bmp", "tsa", "scr", "ico", "res"];
    const COPY_EXT: &[&str] = &["cfg", "txt", "rc"];
    fs::create_dir_all(gamedir).unwrap();
    let Ok(entries) = fs::read_dir(ts) else {
        eprintln!("TS content not found at {} (set TS_DIR)", ts.display());
        exit(1)
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name_s = name.to_string_lossy();
        let dest = gamedir.join(&name);
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let ext = Path::new(&*name_s).extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if is_dir && DIRS.iter().any(|d| d.eq_ignore_ascii_case(&name_s)) || !is_dir && LINK_EXT.contains(&ext.as_str()) {
            // Re-point a link left from another content directory (TS_DIR moved); keep real files.
            match fs::symlink_metadata(&dest) {
                Ok(m) if m.file_type().is_symlink() && fs::read_link(&dest).ok().as_deref() != Some(e.path().as_path()) => {
                    fs::remove_file(&dest).unwrap();
                    std::os::unix::fs::symlink(e.path(), &dest).unwrap();
                }
                Ok(_) => {}
                Err(_) => std::os::unix::fs::symlink(e.path(), &dest).unwrap(),
            }
        } else if !is_dir && COPY_EXT.contains(&ext.as_str()) && !dest.exists() {
            fs::copy(e.path(), &dest).unwrap();
        }
    }
}

/// Steam's engine (HL25 `swds.dll`, and the GoldSrc engines it inherits the code from) reads the
/// game DLL's export names from the file itself: it finds the section *named* `.rdata` and
/// translates the export directory's RVAs with that section's mapping, which is where MSVC puts
/// exports. MinGW's ld emits a separate `.edata` section, so the translation lands on garbage and
/// the engine crashes (or, by luck, reads junk) while loading the library. The Windows loader
/// ignores section names, so rename `.rdata` -> `.rodata` and `.edata` -> `.rdata`.
fn msvc_style_export_section(path: &Path) {
    let mut pe = fs::read(path).unwrap();
    let rd = |b: &[u8], o: usize, n: usize| -> usize { b[o..o + n].iter().rev().fold(0, |a, &x| (a << 8) | x as usize) };
    let nt = rd(&pe, 0x3c, 4);
    assert_eq!(&pe[nt..nt + 4], b"PE\0\0", "{} is not a PE file", path.display());
    let count = rd(&pe, nt + 6, 2);
    let first = nt + 24 + rd(&pe, nt + 20, 2);
    let names: Vec<[u8; 8]> = (0..count).map(|i| pe[first + 40 * i..first + 40 * i + 8].try_into().unwrap()).collect();
    if !names.iter().any(|n| n == b".edata\0\0") {
        return; // already MSVC-style
    }
    for (i, n) in names.iter().enumerate() {
        let new: Option<&[u8; 8]> = match n {
            b".rdata\0\0" => Some(b".rodata\0"),
            b".edata\0\0" => Some(b".rdata\0\0"),
            _ => None,
        };
        if let Some(new) = new {
            pe[first + 40 * i..first + 40 * i + 8].copy_from_slice(new);
        }
    }
    fs::write(path, pe).unwrap();
}

/// Removes symbols and debug info from an installed library (binutils `strip`; the MinGW one for
/// PE files).
fn strip(t: &Target, lib: &Path) {
    let tool = match t.triple {
        "i686-pc-windows-gnu" => "i686-w64-mingw32-strip",
        "x86_64-pc-windows-gnu" => "x86_64-w64-mingw32-strip",
        _ => "strip",
    };
    let ok = Command::new(tool).arg(lib).status().unwrap_or_else(|e| panic!("{tool}: {e}")).success();
    assert!(ok, "{tool} {} failed", lib.display());
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to.parent().unwrap()).unwrap();
    // Remove first so a running engine keeps its mapped copy instead of crashing.
    let _ = fs::remove_file(to);
    fs::copy(from, to).unwrap_or_else(|e| panic!("{} -> {}: {e}", from.display(), to.display()));
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            copy(&entry.path(), &dest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_needed_for_game_commands() {
        let v = |s: &str| Some(std::ffi::OsString::from(s));
        assert!(!has_display(None, None) && !has_display(v(""), v("")));
        assert!(has_display(v(":87"), None) && has_display(None, v("wayland-1")));
        assert!(launches_games("all") && launches_games("ts21") && !launches_games("lint") && !launches_games("relaunch"));
    }

    #[test]
    fn parses_ts_status_lines() {
        let line = "[ts]  #2 Bot Neo          alive hp=100 ar=0 frags=3 deaths=1 cash=1200 team='' held=0x1/3s active=0x1 slow=0.35->0.35 award=0 spree=0 gp=21 org=(0 -480 36)";
        let p = parse_status_line(line).unwrap();
        assert_eq!((p.idx, p.name.as_str(), p.state.as_str(), p.frags, p.deaths), (2, "Bot Neo", "alive", 3, 1));
        assert_eq!((p.held.as_str(), p.active.as_str(), p.slow_target, p.org), ("0x1", "0x1", 0.35, [0.0, -480.0, 36.0]));
        assert_eq!(p.gp, "21");
        let old = parse_status_line(&line.replace(" gp=21", "")).unwrap();
        assert_eq!((old.gp.as_str(), old.org), ("", [0.0, -480.0, 36.0]), "a line without gp (an older server)");
        let text = "XTASK: status a\n[ts] status: Dm\n".to_owned() + line + "\nXTASK: status b\n";
        let dumps = status_dumps(&text);
        assert_eq!((dumps.len(), dumps[0].1.len(), dumps[1].1.len()), (2, 1, 0));
    }

    #[test]
    fn wall_scene_checks() {
        let scene = |fx: &[&str]| {
            let lines: String = fx.iter().map(|l| format!("[23:57:29] [ts fx] {l}\n")).collect();
            format!("[ts fx] decal {{shot1 on entity 0 at [0.0, 0.0, 0.0]\nXTASK: wall \n{lines}XTASK: wall shots \nXTASK: wall done \n")
        };
        let good = [
            "flash at [116.7, -727.4, 45.5] ahead 36.3 off-axis 7.5 muzzle-distance 0.0 direction-dot 1.000",
            "bullet from [100.0, -760.0, 51.5] to [146.7, -632.0, 51.3]",
            "impact on texture type C at [146.7, -632.0, 51.3]",
            "decal {shot4 on entity 0 at [146.7, -632.0, 51.3]",
        ];
        let three: Vec<&str> = good.iter().cycle().take(12).copied().collect();
        assert!(wall_checks(&scene(&three)).iter().all(|c| c.1), "a line before the scene doesn't count");
        // The regression: predicted events played with zero origin and angles.
        let before = [
            "flash at [116.7, -727.4, 45.5] ahead 36.3 off-axis 7.5 muzzle-distance 0.0 direction-dot 0.342",
            "bullet from [0.0, 0.0, 28.0] to [440.0, -0.1, 28.6]",
            "impact on texture type C at [440.0, -0.1, 28.6]",
            "decal {shot5 on entity 0 at [440.0, -0.1, 28.6]",
        ];
        let three: Vec<&str> = before.iter().cycle().take(12).copied().collect();
        assert!(wall_checks(&scene(&three)).iter().all(|c| !c.1));
        assert!(wall_checks(&scene(&[])).iter().all(|c| !c.1), "no shots");
    }

    #[test]
    fn laser_akimbo_addbot_escape_checks() {
        let text = "XTASK: lasers \n\
            [ts fx] laser local gun 0 beam [1, 2, 3] to [4, 5, 6] muzzle [1, 2, 4] ahead 17.6 right 6.8 up 0.3\n\
            [ts fx] laser local gun 1 beam [1, 2, 3] to [4, 5, 6] muzzle [1, 2, 4] ahead 18.0 right -4.9 up -2.0\n\
            [ts fx] laser other #2 beam [-1142.2, -44.2, -21.6] to [-1123.6, -3950.0, 8.7] hand [-1142.3, -28.2, -21.6] origin [-1152.0, 0.0, -20.0] eye [-1152.0, 0.0, 8.0] dir [0.0, -1.0, 0.0]\n\
            XTASK: lasers done \n\
            XTASK: akimbo \n[ts fx] akimbo shot #1 weapon 8 linked\n[ts fx] flash at [0, 0, 0]\n\
            [ts fx] akimbo shot #1 weapon 8 gun b0\n[ts fx] akimbo shot #1 weapon 8 gun b1\nXTASK: akimbo done \n\
            XTASK: addbot \nLe Marc connected with skill 0.500000\n\
            XTASK: escape \n[ts key] cancelselect: escape\n";
        assert!(laser_checks(text).iter().all(|c| c.1), "{:?}", laser_checks(text));
        assert!(akimbo_checks(text)[0].1);
        let dumps = vec![("addbot".to_owned(), vec![parse_status_line("[ts]  #3 Le Marc          alive hp=100 ar=0 frags=0 deaths=0 cash=1200 team='' held=0x0/0s active=0x0 slow=1.00->1.00 award=0 spree=0 org=(0 0 0)").unwrap()])];
        assert!(addbot_checks(text, &dumps)[0].1);
        assert!(escape_checks(text)[0].1);
        // One local beam only (the akimbo bug), a beam from the origin, a dropped second shot.
        let bad =
            text.replace("laser local gun 1", "laser local gun 0").replace("hand [-1142.3, -28.2, -21.6]", "hand [-1152.0, 0.0, -20.0]");
        assert!(laser_checks(&bad).iter().all(|c| !c.1));
        assert!(!akimbo_checks(&text.replace("gun b1\n", "x\n"))[0].1);
        assert!(!addbot_checks(&text.replace("XTASK: addbot", "Bots only in listen servers.\nXTASK: addbot"), &[])[0].1);
    }

    #[test]
    fn secs_uses_the_aliases() {
        // Game seconds: 4.7 s at 2x is 235 frames.
        assert_eq!(TIMESCALE, 2.0);
        assert_eq!(secs(4.7), "w100;w100;w10;w10;w10;wait;wait;wait;wait;wait;\n");
    }
}
