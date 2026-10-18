//! Port addition: restart and reconnect (docs/implementation/client-plugins.md §13.5, issue #301).
//! When the game must restart to get onto a server (a port version the server needs, a plugin
//! update that loads at the next start), [`restart`] tries, in order ([`plan::plan`]):
//!
//! 1. the running tsmod-launcher over IPC (`TSMOD_LAUNCHER_IPC`, [`ipc`]);
//! 2. starting `ts-launcher relaunch ...` ([`launcher::find`]: `TSMOD_LAUNCHER*`, else
//!    `<gamedir>/launcher.toml`);
//! 3. relaunching itself, only on the platforms in [`plan::SELF_RELAUNCH_VERIFIED`] and never for
//!    a port update;
//! 4. the quit-and-inform card, which copies the address to the clipboard and writes
//!    [`reconnect::PATH`] for the next start's "Reconnect to <server>?" card.
//!
//! The password is never passed on. Every request carries only `password_required` (whether the
//! `password` cvar was set), and a relaunched game gets `+tsrs_reconnect <address> password`,
//! which asks for it in a card ([`card::Card::password`]) before connecting.
//!
//! Commands (port additions): `tsrs_reconnect <address> [password]` (what the relaunch runs; it
//! can do nothing a stuffed `connect` can't), and the dev aid `ts_dev_restart [restart | port
//! <requirement> [<url>] | plugin <id@version>]`, which requests a restart from the server the
//! client is on (op 5's card and plugin updates call [`restart`] too: `crate::plugins::offers`, #299).

pub mod args;
pub mod card;
#[cfg(feature = "e2e")]
mod e2e;
pub mod ipc;
pub mod launcher;
pub mod plan;
pub mod reconnect;

use card::{Answer, Card};
use plan::{Platform, SelfRelaunch, Step};
pub use plan::{Reason, Request};

use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
use reconnect::Reconnect;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Frames after start before the `.reconnect` card is offered, so the command line's own
/// `+connect` or `+tsrs_reconnect` runs first.
const OFFER_AFTER_FRAMES: u32 = 30;

struct Start {
    frames: Cell<u32>,
    /// A `tsrs_reconnect` or a reconnect already ran: `.reconnect` is only removed.
    handled: Cell<bool>,
}
static START: Global<Start> = Global(Start { frames: Cell::new(0), handled: Cell::new(false) });

pub fn init() {
    let e = engine();
    e.add_command(c"tsrs_reconnect", cmd_reconnect);
    e.add_command(c"ts_dev_restart", cmd_dev_restart);
}

/// The server the client is on: its address and name. `None` when not connected, or on a listen
/// server (`loopback` isn't an address another process can join).
pub fn current_server() -> Option<(String, String)> {
    let (connected, address) = engine().net_status()?;
    if !connected || !args::valid_address(&address) {
        return None;
    }
    Some((address, crate::hud::with(|h| h.server_name.clone())))
}

/// Whether the `password` cvar is set. Only this is ever read from it here.
fn password_set() -> bool {
    Cvar::find(c"password").string().is_some_and(|s| !s.to_bytes().is_empty())
}

/// Restarts the game and reconnects to the current server, for `reason`. The entry point for op
/// 5 (`port_version`) and plugin restarts.
pub fn restart(reason: Reason) -> Result<(), String> {
    let (address, name) = current_server().ok_or("not connected to a server")?;
    let req = Request::new(&address, &name, password_set(), reason)?;
    request(&req);
    Ok(())
}

/// Runs the first step of [`plan::plan`] that works and quits; otherwise shows the
/// quit-and-inform card.
pub fn request(req: &Request) {
    let e = engine();
    let cx = context();
    for step in plan::plan(req, &cx) {
        e.print(&format!("[ts] relaunch: {}\n", describe(&step)));
        match run(&step) {
            Ok(()) => {
                e.print("[ts] relaunch: done; quitting\n");
                e.client_cmd("quit\n");
                return;
            }
            Err(err) => e.print(&format!("[ts] relaunch: that failed ({err}); trying the next way\n")),
        }
    }
    inform(req);
}

fn describe(step: &Step) -> String {
    match step {
        Step::Ipc { endpoint, line } => format!("asking the running launcher ({endpoint}): {}", line.trim_end()),
        Step::Launcher { exe, args } => format!("starting the launcher: {} {}", exe.display(), args.join(" ")),
        Step::Exec { exe, args } => format!("restarting the game: {} {}", exe.display(), args.join(" ")),
        Step::Steam { url } => format!("restarting through Steam: {url}"),
    }
}

fn root_and_game_dir() -> (PathBuf, PathBuf, String) {
    let root = std::env::current_dir().unwrap_or_default();
    let name = engine().game_directory();
    (root.clone(), root.join(&name), name)
}

fn context() -> plan::Context {
    let env = launcher::EnvVars::from_env();
    let (root, game_dir, gamedir_name) = root_and_game_dir();
    let toml = std::fs::read_to_string(game_dir.join(launcher::FILE)).ok();
    let (launcher, notes) = launcher::find(&env, &root, &game_dir, toml.as_deref());
    for n in notes {
        engine().print(&format!("[ts] relaunch: {n}\n"));
    }
    plan::Context { pid: std::process::id(), ipc: env.ipc, launcher, self_relaunch: self_relaunch(gamedir_name) }
}

fn platform() -> Platform {
    match (engine().is_xash(), cfg!(windows)) {
        (true, false) => Platform::FwgsLinux,
        (true, true) => Platform::FwgsWindows,
        (false, false) => Platform::SteamLinux,
        (false, true) => Platform::SteamWindows,
    }
}

/// The relaunch e2e's override of [`plan::self_relaunch_enabled`]; never set in a shipped build.
fn forced_self_relaunch() -> Option<bool> {
    #[cfg(feature = "e2e")]
    return e2e::self_relaunch();
    #[cfg(not(feature = "e2e"))]
    None
}

fn self_relaunch(gamedir: String) -> Option<SelfRelaunch> {
    let p = platform();
    let forced = forced_self_relaunch();
    if !forced.unwrap_or_else(|| plan::self_relaunch_enabled(p)) {
        let why = if forced.is_some() { "turned off for this test" } else { "no passing relaunch test yet" };
        engine().print(&format!("[ts] relaunch: self-relaunch isn't enabled on {p:?} ({why})\n"));
        return None;
    }
    if !engine().is_xash() {
        return Some(SelfRelaunch::Steam { gamedir });
    }
    Some(SelfRelaunch::Exec { exe: std::env::current_exe().ok()?, original: original_args() })
}

/// The engine's own arguments, without the program: `/proc/self/cmdline` on Linux, the parsed
/// `GetCommandLineW` (std's `args`) elsewhere.
fn original_args() -> Vec<String> {
    #[cfg(target_os = "linux")]
    if let Ok(b) = std::fs::read("/proc/self/cmdline") {
        let b = b.strip_suffix(&[0]).unwrap_or(&b);
        return b.split(|&c| c == 0).skip(1).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    }
    std::env::args().skip(1).collect()
}

fn run(step: &Step) -> Result<(), String> {
    match step {
        Step::Ipc { endpoint, line } => ipc::send(endpoint, line),
        Step::Launcher { exe, args } => spawn(exe, args, false),
        Step::Exec { exe, args } => spawn(exe, args, true),
        Step::Steam { url } => {
            if cfg!(windows) {
                spawn(Path::new("rundll32"), &["url.dll,FileProtocolHandler".into(), url.clone()], false)
            } else {
                spawn(Path::new("xdg-open"), std::slice::from_ref(url), false)
            }
        }
    }
}

/// Starts `exe` without waiting for it. The game's console output goes to the new game
/// (`inherit`); the launcher's is dropped. Either outlives this process (its own process group).
fn spawn(exe: &Path, args: &[String], inherit: bool) -> Result<(), String> {
    let mut c = Command::new(exe);
    c.args(args).stdin(Stdio::null());
    if !inherit {
        c.stdout(Stdio::null()).stderr(Stdio::null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        c.creation_flags(if inherit { CREATE_NEW_PROCESS_GROUP } else { DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP });
    }
    c.spawn().map(|_| ()).map_err(|e| format!("{}: {e}", exe.display()))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn reconnect_path() -> PathBuf {
    root_and_game_dir().1.join(reconnect::PATH)
}

/// Writes `.reconnect` (to a temporary file, then renamed over it).
fn write_reconnect(req: &Request) -> std::io::Result<()> {
    let path = reconnect_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, Reconnect::new(&req.connect, &req.server_name, req.password_required, now_secs()).to_text())?;
    std::fs::rename(&tmp, &path)
}

/// Case 4: the card that tells the player how to come back, and quits if they choose.
fn inform(req: &Request) {
    let e = engine();
    let clipboard = goldsrc::platform::sdl::set_clipboard(&req.connect);
    if let Err(err) = write_reconnect(req) {
        e.print(&format!("[ts] relaunch: can't write {}: {err}\n", reconnect::PATH));
    }
    if card::show(&Card::inform(req, clipboard)) == Answer::Ok(None) {
        e.client_cmd("quit\n");
    }
}

/// Connects to `address`, setting the `password` cvar first when one was typed in a card. Both
/// were checked: the address by [`args::valid_address`], the password by the card (no quotes,
/// no control characters).
fn connect(address: &str, password: Option<&str>) {
    START.0.handled.set(true);
    let mut cmd = String::new();
    if let Some(pw) = password {
        cmd += &format!("password \"{pw}\"\n");
    }
    cmd += &format!("connect {address}\n");
    engine().client_cmd(&cmd);
}

/// Called every frame (`HUD_Frame`): once, shortly after start, offers `.reconnect`.
pub fn frame() {
    let s = &START.0;
    let n = s.frames.get();
    if n > OFFER_AFTER_FRAMES {
        return;
    }
    s.frames.set(n + 1);
    if n == OFFER_AFTER_FRAMES {
        offer_reconnect();
    }
}

fn offer_reconnect() {
    let path = reconnect_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    // Read once: whatever happens now, it isn't offered again.
    let _ = std::fs::remove_file(&path);
    let e = engine();
    if START.0.handled.get() || engine().net_status().is_some_and(|(connected, _)| connected) {
        return;
    }
    let Some(r) = Reconnect::parse(&text) else {
        e.print(&format!("[ts] relaunch: {} isn't readable; ignored\n", reconnect::PATH));
        return;
    };
    if !r.fresh(now_secs()) {
        e.print(&format!("[ts] relaunch: {} is older than {} minutes; not offered\n", reconnect::PATH, reconnect::FRESH_SECS / 60));
        return;
    }
    match card::show(&Card::reconnect(&r)) {
        Answer::Ok(pw) => connect(&r.address, pw.as_deref()),
        Answer::Cancel => e.print("[ts] relaunch: reconnect declined\n"),
    }
}

fn argv() -> Vec<String> {
    let e = engine();
    (1..e.cmd_argc()).map(|i| e.cmd_argv(i).to_string_lossy().into_owned()).collect()
}

/// `tsrs_reconnect <address> [password]` (port addition).
extern "C" fn cmd_reconnect() {
    let e = engine();
    let a = argv();
    let refs: Vec<&str> = a.iter().map(String::as_str).collect();
    match args::parse_reconnect(&refs) {
        Err(usage) => e.print(&format!("{usage}\n")),
        Ok((address, false)) => connect(&address, None),
        Ok((address, true)) => {
            START.0.handled.set(true);
            match card::show(&Card::password(&address)) {
                Answer::Ok(Some(pw)) => connect(&address, Some(&pw)),
                _ => e.print("[ts] relaunch: reconnect cancelled\n"),
            }
        }
    }
}

/// `ts_dev_restart [restart | port <requirement> [<url>] | plugin <id@version>]` (port dev aid).
extern "C" fn cmd_dev_restart() {
    let e = engine();
    let a = argv();
    let reason = match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["restart"] => Reason::Restart,
        ["port", req] => Reason::Port { requirement: (*req).into(), url: String::new() },
        ["port", req, url] => Reason::Port { requirement: (*req).into(), url: (*url).into() },
        ["plugin", p] => Reason::Plugin { plugin: (*p).into() },
        _ => {
            e.print("usage: ts_dev_restart [restart | port <requirement> [<url>] | plugin <id@version>]\n");
            return;
        }
    };
    if let Err(err) = restart(reason) {
        e.print(&format!("[ts] relaunch: {err}\n"));
    }
}
