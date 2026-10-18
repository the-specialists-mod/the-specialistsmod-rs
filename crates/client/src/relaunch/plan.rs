//! What a restart asks for, and the order of the ways to do it (docs/implementation/client-plugins.md
//! §13.5): 1. the running launcher over IPC, 2. starting `ts-launcher relaunch`, 3. the game
//! relaunching itself (only where its e2e test passes, never for a port update), 4. quit and
//! inform (the caller's fallback when every step fails or none applies).

use super::{args, ipc};
use std::path::PathBuf;

/// Why the game must restart. The names are the IPC request's `reason` and the `--reason` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Only restart and reconnect (a plugin that was approved but loads at the next start, or the
    /// dev command).
    Restart,
    /// The server needs a port version this client isn't (op 5): a semver requirement and where
    /// to get it. Only the launcher can update the port, and it asks the player first.
    Port { requirement: String, url: String },
    /// A loaded plugin must change version (`tspkg ... restart`, §13.3): `id@version`, shown only.
    Plugin { plugin: String },
}

impl Reason {
    pub fn name(&self) -> &'static str {
        match self {
            Reason::Restart => "restart",
            Reason::Port { .. } => "port",
            Reason::Plugin { .. } => "plugin",
        }
    }
}

/// A restart request. There is deliberately no password here: only whether the `password` cvar
/// was set when the restart became necessary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// `host:port` ([`args::valid_address`]).
    pub connect: String,
    /// The server's name, for the cards and the launcher's dialog (≤ 64 bytes, no control
    /// characters); empty when unknown.
    pub server_name: String,
    pub password_required: bool,
    pub reason: Reason,
}

/// The longest version requirement (op 5's limit, §13.3).
pub const MAX_REQUIREMENT: usize = 48;
/// The longest URL (op 5's limit).
pub const MAX_URL: usize = 96;
/// The longest server name passed on.
pub const MAX_SERVER_NAME: usize = 64;

impl Request {
    /// Checks every field: the address, a requirement that parses as semver, a URL and a plugin
    /// of printable ASCII without spaces or quotes. The server name is cleaned instead (control
    /// characters dropped, cut at a character boundary).
    pub fn new(connect: &str, server_name: &str, password_required: bool, reason: Reason) -> Result<Request, String> {
        if !args::valid_address(connect) {
            return Err(format!("not a host:port address: {connect:?}"));
        }
        let plain = |s: &str, max: usize| s.len() <= max && s.bytes().all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\');
        match &reason {
            Reason::Restart => {}
            Reason::Port { requirement, url } => {
                if requirement.len() > MAX_REQUIREMENT || semver::VersionReq::parse(requirement).is_err() {
                    return Err(format!("not a version requirement: {requirement:?}"));
                }
                if !plain(url, MAX_URL) {
                    return Err(format!("not a URL: {url:?}"));
                }
            }
            Reason::Plugin { plugin } => {
                if plugin.is_empty() || !plugin.contains('@') || !plain(plugin, 64) {
                    return Err(format!("not id@version: {plugin:?}"));
                }
            }
        }
        let mut name: String = server_name.chars().filter(|c| !c.is_control()).collect();
        while name.len() > MAX_SERVER_NAME {
            name.pop();
        }
        Ok(Request { connect: connect.to_owned(), server_name: name.trim().to_owned(), password_required, reason })
    }

    /// The server as the cards name it: "Bob's TS (203.0.113.5:27015)", or the address.
    pub fn server_label(&self) -> String {
        if self.server_name.is_empty() { self.connect.clone() } else { format!("{} ({})", self.server_name, self.connect) }
    }
}

/// A launcher found for this installation ([`super::launcher::find`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    pub exe: PathBuf,
    pub instance: String,
}

/// How the game relaunches itself, when that is enabled on this engine and platform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelfRelaunch {
    /// FWGS: run the engine binary again with these arguments (original parameters, no commands).
    Exec { exe: PathBuf, original: Vec<String> },
    /// Steam HL25: open `steam://run/70//...` once the game has quit.
    Steam { gamedir: String },
}

/// What the client knows when a restart is needed.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// This process's id (the launcher waits for it to exit).
    pub pid: u32,
    /// `TSMOD_LAUNCHER_IPC`, when set.
    pub ipc: Option<String>,
    pub launcher: Option<Launcher>,
    /// `None` when self-relaunch isn't enabled here.
    pub self_relaunch: Option<SelfRelaunch>,
}

/// One way to restart, in the order they are tried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// 1: send `line` to the running launcher at `endpoint`; it answers `{"ok": true}`.
    Ipc { endpoint: String, line: String },
    /// 2: start the launcher detached with `relaunch` arguments.
    Launcher { exe: PathBuf, args: Vec<String> },
    /// 3: start the engine again.
    Exec { exe: PathBuf, args: Vec<String> },
    /// 3 on Steam: open the URL (with `xdg-open` or the Windows URL handler).
    Steam { url: String },
}

/// The steps for `req`, in order; the caller tries each until one works, and falls back to quit
/// and inform (case 4) when none does or the list is empty.
pub fn plan(req: &Request, cx: &Context) -> Vec<Step> {
    let mut steps = Vec::new();
    let instance = cx.launcher.as_ref().map(|l| l.instance.as_str());
    if let Some(endpoint) = &cx.ipc {
        steps.push(Step::Ipc { endpoint: endpoint.clone(), line: ipc::request_line(req, instance, cx.pid) });
    }
    if let Some(l) = &cx.launcher {
        steps.push(Step::Launcher { exe: l.exe.clone(), args: args::launcher_args(req, &l.instance, cx.pid) });
    }
    // Updating the port always needs the launcher: relaunching the same build would only be
    // refused again.
    if !matches!(req.reason, Reason::Port { .. }) {
        match &cx.self_relaunch {
            Some(SelfRelaunch::Exec { exe, original }) => steps.push(Step::Exec { exe: exe.clone(), args: args::self_args(original, req) }),
            Some(SelfRelaunch::Steam { gamedir }) => steps.push(Step::Steam { url: args::steam_url(gamedir, req) }),
            None => {}
        }
    }
    steps
}

/// The engine and platform a self-relaunch runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    FwgsLinux,
    /// FWGS on Windows. The client can't tell Wine from native Windows (both are a win32
    /// `client.dll` in the same engine), so this covers both, but it was verified only under Wine
    /// (`cargo xtask relaunch fwgs-win`, FWGS 0.21 win32).
    FwgsWindows,
    SteamLinux,
    SteamWindows,
}

/// The platforms whose self-relaunch passed its e2e test (`cargo xtask relaunch`, docs/validation.md
/// §2.r): a platform goes in only after its test does, and comes out when it fails (§13.5 case
/// 3). Everywhere else the client goes from the launcher straight to quit and inform.
///
/// * `FwgsLinux`: the `self` scenario on `fwgs` passed, 2026-10-19 (#332).
/// * `FwgsWindows`: the `self` scenario on `fwgs-win` (FWGS 0.21 win32 under Wine) passed, the
///   same runs. Not run on native Windows; [`Platform`] doesn't tell the two apart.
/// * Steam HL25 (Linux and Windows): no test yet (#307), so off.
pub const SELF_RELAUNCH_VERIFIED: &[Platform] = &[Platform::FwgsLinux, Platform::FwgsWindows];

pub fn self_relaunch_enabled(p: Platform) -> bool {
    SELF_RELAUNCH_VERIFIED.contains(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launcher() -> Launcher {
        Launcher { exe: PathBuf::from("/opt/ts-launcher"), instance: "ab12".into() }
    }
    fn exec() -> SelfRelaunch {
        SelfRelaunch::Exec { exe: PathBuf::from("/games/xash/xash3d"), original: vec!["-game".into(), "ts".into()] }
    }
    fn kinds(steps: &[Step]) -> Vec<&'static str> {
        steps
            .iter()
            .map(|s| match s {
                Step::Ipc { .. } => "ipc",
                Step::Launcher { .. } => "launcher",
                Step::Exec { .. } => "exec",
                Step::Steam { .. } => "steam",
            })
            .collect()
    }

    #[test]
    fn decision_order() {
        let req = Request::new("203.0.113.5:27015", "", false, Reason::Restart).unwrap();
        let full = Context {
            pid: 9,
            ipc: Some("/run/user/1000/ts-launcher-1000.sock".into()),
            launcher: Some(launcher()),
            self_relaunch: Some(exec()),
        };
        assert_eq!(kinds(&plan(&req, &full)), ["ipc", "launcher", "exec"]);
        let no_ipc = Context { ipc: None, ..full.clone() };
        assert_eq!(kinds(&plan(&req, &no_ipc)), ["launcher", "exec"]);
        let only_self = Context { launcher: None, ..no_ipc.clone() };
        assert_eq!(kinds(&plan(&req, &only_self)), ["exec"]);
        let steam = Context { self_relaunch: Some(SelfRelaunch::Steam { gamedir: "ts".into() }), ..only_self.clone() };
        assert_eq!(kinds(&plan(&req, &steam)), ["steam"]);
        // Nothing: the caller shows the quit-and-inform card.
        assert!(plan(&req, &Context::default()).is_empty());
    }

    /// The password is never passed on: not in the IPC line, the launcher's or the game's
    /// arguments, the Steam URL or `.reconnect`, even when the original command line had one.
    /// Only `password_required` travels.
    #[test]
    fn no_password_anywhere() {
        const PW: &str = "hunter2";
        let req = Request::new("203.0.113.5:27015", "Bob's TS", true, Reason::Plugin { plugin: "webview@0.2.0".into() }).unwrap();
        let original: Vec<String> = ["-game", "ts", "+password", PW, "+connect", "203.0.113.5:27015"].map(String::from).to_vec();
        let mut words = Vec::new();
        for self_relaunch in [SelfRelaunch::Exec { exe: "/x/xash3d".into(), original }, SelfRelaunch::Steam { gamedir: "ts".into() }] {
            let cx = Context { pid: 9, ipc: Some("/run/x.sock".into()), launcher: Some(launcher()), self_relaunch: Some(self_relaunch) };
            for step in plan(&req, &cx) {
                match step {
                    Step::Ipc { line, .. } => {
                        assert!(line.contains(r#""password_required":true"#), "{line}");
                        words.push(line);
                    }
                    Step::Launcher { args, .. } => {
                        assert!(args.contains(&"--password-required".to_owned()));
                        words.extend(args);
                    }
                    Step::Exec { args, .. } => {
                        assert!(args.ends_with(&["+tsrs_reconnect".into(), "203.0.113.5:27015".into(), "password".into()]), "{args:?}");
                        words.extend(args);
                    }
                    Step::Steam { url } => words.push(url),
                }
            }
        }
        words.push(super::super::reconnect::Reconnect::new(&req.connect, &req.server_name, req.password_required, 1).to_text());
        for w in &words {
            assert!(!w.contains(PW), "the password leaked into {w:?}");
        }
    }

    #[test]
    fn port_updates_need_the_launcher() {
        let req = Request::new("203.0.113.5:27015", "", false, Reason::Port { requirement: ">=3.0.8".into(), url: String::new() }).unwrap();
        let cx = Context { pid: 9, ipc: None, launcher: Some(launcher()), self_relaunch: Some(exec()) };
        assert_eq!(kinds(&plan(&req, &cx)), ["launcher"]);
        let cx = Context { launcher: None, ..cx };
        assert!(plan(&req, &cx).is_empty());
    }

    /// Only the platforms whose `cargo xtask relaunch` self scenario passed (#332). Steam needs
    /// its own test (#307) before it may go in.
    #[test]
    fn self_relaunch_is_off_until_verified() {
        assert!(self_relaunch_enabled(Platform::FwgsLinux));
        assert!(self_relaunch_enabled(Platform::FwgsWindows));
        for p in [Platform::SteamLinux, Platform::SteamWindows] {
            assert!(!self_relaunch_enabled(p), "{p:?} has no passing relaunch test yet");
        }
    }

    #[test]
    fn requests_are_checked() {
        assert!(Request::new("1.2.3.4:27015;quit", "", false, Reason::Restart).is_err());
        assert!(Request::new("1.2.3.4:27015", "", false, Reason::Port { requirement: "not semver".into(), url: String::new() }).is_err());
        assert!(Request::new("1.2.3.4:27015", "", false, Reason::Port { requirement: ">=3.0.8".into(), url: "a b".into() }).is_err());
        assert!(Request::new("1.2.3.4:27015", "", false, Reason::Plugin { plugin: "webview".into() }).is_err());
        let r = Request::new("1.2.3.4:27015", "  Bob's\n TS\u{7}", false, Reason::Restart).unwrap();
        assert_eq!(r.server_name, "Bob's TS");
        assert_eq!(r.server_label(), "Bob's TS (1.2.3.4:27015)");
        let long = "é".repeat(40);
        assert!(Request::new("1.2.3.4:27015", &long, false, Reason::Restart).unwrap().server_name.len() <= MAX_SERVER_NAME);
    }
}
