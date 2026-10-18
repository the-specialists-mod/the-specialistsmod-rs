//! `cargo xtask steam [listen | <server-root>...]`: the Steam Half-Life 25 client (Linux, 32-bit
//! `hl_linux`) scripted like `play`, as a listen server or joining a Linux dedicated-server root
//! (issue #5, old #67; `docs/validation.md` §5).
//!
//! The game needs Steam running and logged in, and only one Half-Life client can run at a time.
//! xtask starts Steam (`steam -silent`) when it has a saved login, and stops with a message when
//! it can't log in on its own. While the game runs, the user's friends status is set to invisible
//! and put back afterwards ([`Invisible`]). The game is started through the Steam runtime with
//! `SteamAppId=70`, never `steam -applaunch` (that hangs while the desktop is locked). Its console
//! goes to `Half-Life/qconsole.log` (`-condebug`), screenshots to `Half-Life/ts/HalfLifeNN.tga`.
//!
//! On top of `play`'s scenes the scripts check the regressions that only showed on this client:
//! a black world (old #62, the `IClientVGUI` export), server-set view angles held while a menu
//! owns the cursor, click-to-spawn after a `changelevel`, and third person after a respawn
//! (old #23).

use crate::roots::{EXEC_SERVER_CFG, Engine, Os, Root, Target};
use crate::{Ctx, PlayerStatus, Run, a2s, crashed, say, status, status_dumps};
use crate::{
    addbot_checks, akimbo_checks, click, escape_checks, laser_checks, script_addbot, script_escape, script_join_and_spawn,
    script_lasers_and_akimbo, script_stopped_laser_bot, script_wall_shots, secs, shot, wall_checks,
};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio, exit};
use std::time::{Duration, Instant};
use std::{env, fs};

/// Half-Life's Steam app id.
const HALF_LIFE_APP: &str = "70";
/// First port for the dedicated servers (`test` uses 27115.., `client` 27150, `play` 27215..).
const DEDICATED_PORT: u16 = 27160;
/// The listen server's port.
const LISTEN_PORT: u16 = 27170;
/// First port for the 2.1 servers ([`Ctx::steam_ts21`]).
const TS21_PORT: u16 = 27180;
/// The script's end marker (`ts_dev_touch`, in the game directory that runs it).
const MARKER: &str = "xtask_steam_done";
/// How long to wait for Steam to log in after starting it.
const LOGIN_WAIT: Duration = Duration::from_secs(180);

// ---------------------------------------------------------------------------------------------
// Valve KeyValues (VDF) text: libraryfolders.vdf, registry.vdf, loginusers.vdf, localconfig.vdf.

/// A parsed VDF value: a string or a block of `"key" value` pairs.
#[derive(Debug, PartialEq)]
enum Vdf {
    Str(String),
    Block(Vec<(String, Vdf)>),
}

impl Vdf {
    fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Block(kv) => kv.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }

    /// The value at a `/`-separated key path.
    fn path(&self, path: &str) -> Option<&Vdf> {
        path.split('/').try_fold(self, |v, k| v.get(k))
    }

    fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Block(_) => None,
        }
    }

    /// The first value named `key` anywhere below this one (depth first).
    fn find(&self, key: &str) -> Option<&Vdf> {
        let Vdf::Block(kv) = self else { return None };
        kv.iter().find_map(|(k, v)| if k.eq_ignore_ascii_case(key) { Some(v) } else { v.find(key) })
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Open,
    Close,
    S(String),
}

fn vdf_tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '/' if it.peek() == Some(&'/') => {
                for c in it.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => match it.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(c) => s.push(c),
                            None => {}
                        },
                        c => s.push(c),
                    }
                }
                out.push(Tok::S(s));
            }
            c if c.is_whitespace() => {}
            c => {
                let mut s = c.to_string();
                while let Some(&c) = it.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    it.next();
                }
                out.push(Tok::S(s));
            }
        }
    }
    out
}

fn vdf_block(toks: &[Tok], i: &mut usize) -> Vec<(String, Vdf)> {
    let mut kv = Vec::new();
    while let Some(Tok::S(key)) = toks.get(*i) {
        *i += 1;
        match toks.get(*i) {
            Some(Tok::Open) => {
                *i += 1;
                let block = vdf_block(toks, i);
                if toks.get(*i) == Some(&Tok::Close) {
                    *i += 1;
                }
                kv.push((key.clone(), Vdf::Block(block)));
            }
            Some(Tok::S(v)) => {
                *i += 1;
                kv.push((key.clone(), Vdf::Str(v.clone())));
            }
            _ => break,
        }
    }
    kv
}

fn parse_vdf(text: &str) -> Vdf {
    Vdf::Block(vdf_block(&vdf_tokens(text), &mut 0))
}

fn read_vdf(path: &Path) -> Option<Vdf> {
    fs::read_to_string(path).ok().map(|t| parse_vdf(&t))
}

/// The Steam library folders listed in `libraryfolders.vdf`, the ones that have `app` installed
/// first. Handles the current format (`"0" { "path" "..." "apps" { "70" "..." } }`) and the old
/// one (`"1" "/path"`).
fn library_folders(vdf: &str, app: &str) -> Vec<PathBuf> {
    let root = parse_vdf(vdf);
    let Some(Vdf::Block(libs)) = root.get("libraryfolders") else { return Vec::new() };
    let mut with_app = Vec::new();
    let mut others = Vec::new();
    for (key, lib) in libs {
        match lib {
            Vdf::Str(path) if key.chars().all(|c| c.is_ascii_digit()) => others.push(PathBuf::from(path)),
            Vdf::Block(_) => {
                let Some(Vdf::Str(path)) = lib.get("path") else { continue };
                if lib.get("apps").and_then(|a| a.get(app)).is_some() {
                    with_app.push(PathBuf::from(path));
                } else {
                    others.push(PathBuf::from(path));
                }
            }
            Vdf::Str(_) => {}
        }
    }
    with_app.extend(others);
    with_app
}

// ---------------------------------------------------------------------------------------------
// Steam: where it is, whether it runs and is logged in, the friends status.

fn home() -> PathBuf {
    PathBuf::from(env::var("HOME").unwrap_or_default())
}

/// The Steam install and the Half-Life folder in it.
pub struct Steam {
    /// Steam's root (`ubuntu12_32/steam-runtime/run.sh` lives there).
    root: PathBuf,
    /// `.../steamapps/common/Half-Life`.
    hl: PathBuf,
}

impl Steam {
    /// Finds Steam in `~/.local/share/Steam` or `~/.steam/steam`, and Half-Life in its library
    /// folders (`TS_STEAM_HL` overrides the Half-Life folder).
    fn locate() -> Result<Steam, String> {
        let home = home();
        let candidates = [home.join(".local/share/Steam"), home.join(".steam/steam"), home.join(".steam/root")];
        let root =
            candidates.iter().find(|r| r.join("ubuntu12_32/steam-runtime/run.sh").exists()).cloned().ok_or_else(|| {
                "Steam not found in ~/.local/share/Steam or ~/.steam/steam (no ubuntu12_32/steam-runtime/run.sh)".to_owned()
            })?;
        let hl = match env::var_os("TS_STEAM_HL") {
            Some(p) => PathBuf::from(p),
            None => {
                let vdf = ["steamapps/libraryfolders.vdf", "config/libraryfolders.vdf"]
                    .iter()
                    .find_map(|f| fs::read_to_string(root.join(f)).ok())
                    .unwrap_or_default();
                let mut libs = library_folders(&vdf, HALF_LIFE_APP);
                libs.push(root.clone());
                libs.into_iter().map(|l| l.join("steamapps/common/Half-Life")).find(|p| p.join("hl_linux").exists()).ok_or_else(|| {
                    format!("Half-Life (app {HALF_LIFE_APP}) not found in the Steam libraries; install it or set TS_STEAM_HL")
                })?
            }
        };
        if !hl.join("hl_linux").exists() {
            return Err(format!("{}: no hl_linux (TS_STEAM_HL must point at the Half-Life folder)", hl.display()));
        }
        Ok(Steam { root, hl })
    }

    /// The logged-in account: the 32-bit account id, which names `userdata/<id>`. Older Steam
    /// clients keep it in `~/.steam/registry.vdf` (`ActiveProcess/ActiveUser`, 0 when logged
    /// out); the current Linux client doesn't write that key, so the fallback is the state of the
    /// last line of `logs/connection_log.txt` (`[Logged On, 4, 7] [U:1:<id>] ...`; Steam logs
    /// `[Logged Off, ...]` when it logs off or exits). Only meaningful while Steam runs.
    fn active_user(&self) -> Option<u32> {
        let reg = read_vdf(&home().join(".steam/registry.vdf"));
        let id = reg.as_ref().and_then(|r| r.path("Registry/HKCU/Software/Valve/Steam/ActiveProcess/ActiveUser")?.str()?.parse().ok());
        if let Some(id @ 1..) = id {
            return Some(id);
        }
        let log = fs::read(self.root.join("logs/connection_log.txt")).ok()?;
        logged_on_account(&String::from_utf8_lossy(&log[log.len().saturating_sub(64 * 1024)..]))
    }

    /// The account Steam logs in by itself: `AutoLoginUser` from the registry, with a remembered
    /// password in `config/loginusers.vdf`.
    fn auto_login_user(&self) -> Option<String> {
        let reg = read_vdf(&home().join(".steam/registry.vdf"))?;
        let user = reg.path("Registry/HKCU/Software/Valve/Steam/AutoLoginUser")?.str()?.to_owned();
        let users = read_vdf(&self.root.join("config/loginusers.vdf"))?;
        let Some(Vdf::Block(list)) = users.get("users") else { return None };
        list.iter()
            .any(|(_, u)| {
                u.get("AccountName").and_then(Vdf::str) == Some(user.as_str())
                    && u.get("RememberPassword").and_then(Vdf::str) == Some("1")
                    && u.get("WantsOfflineMode").and_then(Vdf::str) != Some("1")
            })
            .then_some(user)
    }

    /// The friends status the user picked (`ePersonaState` in the `FriendStoreLocalPrefs_<id>`
    /// JSON of `userdata/<id>/config/localconfig.vdf`). Steam rewrites the file when the status
    /// changes, so this is the current one (`docs/validation.md` §5.2).
    fn persona_state(&self, account: u32) -> Option<u8> {
        let cfg = read_vdf(&self.root.join(format!("userdata/{account}/config/localconfig.vdf")))?;
        persona_from_prefs(cfg.find(&format!("FriendStoreLocalPrefs_{account}"))?.str()?)
    }

    /// Asks the running Steam to change the friends status (`steam steam://friends/status/<s>`,
    /// which hands the URL to the running client and exits).
    fn set_status(&self, status: &str) -> bool {
        let url = format!("steam://friends/status/{status}");
        let launcher = if Command::new("steam").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
            PathBuf::from("steam")
        } else {
            self.root.join("steam.sh")
        };
        let Ok(mut c) = Command::new(&launcher).arg(&url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() else {
            return false;
        };
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(30) {
            if let Ok(Some(s)) = c.try_wait() {
                return s.success();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = c.kill();
        let _ = c.wait();
        false
    }
}

/// The account of the last `[<state>, n, n] [U:1:<id>]` line of Steam's connection log, if that
/// state is `Logged On`.
fn logged_on_account(log: &str) -> Option<u32> {
    let line = log.lines().rev().find(|l| l.contains("] [U:1:") && l.split("] [").nth(1).is_some_and(|s| s.contains(", ")))?;
    let state = line.split("] [").nth(1)?.split(',').next()?;
    let id: u32 = line.split("[U:1:").nth(1)?.split(']').next()?.parse().ok()?;
    (state == "Logged On" && id != 0).then_some(id)
}

/// `ePersonaState` from Steam's `FriendStoreLocalPrefs` JSON (`{"ePersonaState":1,...}`).
fn persona_from_prefs(json: &str) -> Option<u8> {
    let rest = &json[json.find("\"ePersonaState\"")? + "\"ePersonaState\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// The `steam://friends/status/<name>` for a Steam `EPersonaState`.
fn status_name(state: u8) -> Option<&'static str> {
    Some(match state {
        0 => "offline",
        1 => "online",
        2 => "busy",
        3 => "away",
        4 => "snooze",
        5 => "trade",
        6 => "play",
        7 => "invisible",
        _ => return None,
    })
}

fn running(name: &str) -> bool {
    Command::new("pgrep").args(["-x", name]).stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Steam is running and logged in: the account id. With `start`, a Steam that isn't running is
/// started (`steam -silent`) when it can log in by itself, and given [`LOGIN_WAIT`] to do so.
fn ensure_logged_in(steam: &Steam, start: bool, log: &Path) -> Result<u32, String> {
    if running("steam") {
        // Just started by someone else: give the login a moment.
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(if start { 60 } else { 2 }) {
            if let Some(id) = steam.active_user() {
                return Ok(id);
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        return Err("Steam is running but not logged in: log in to Steam (the account that owns Half-Life), then run this again".into());
    }
    if !start {
        return Err("Steam isn't running".into());
    }
    let Some(user) = steam.auto_login_user() else {
        return Err(
            "Steam isn't running and has no saved login to start with (AutoLoginUser with a remembered password): start Steam and log in, then run this again"
                .into(),
        );
    };
    say!("== steam: starting Steam (steam -silent) to log in as {user}; log: {}", log.display());
    let out = fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so a Ctrl-C meant for xtask doesn't stop Steam; it keeps running.
        Command::new("steam")
            .arg("-silent")
            .stdin(Stdio::null())
            .stdout(out.try_clone().map_err(|e| e.to_string())?)
            .stderr(out)
            .process_group(0)
            .spawn()
            .map_err(|e| format!("couldn't start steam: {e}"))?;
    }
    let t = Instant::now();
    while t.elapsed() < LOGIN_WAIT {
        if let Some(id) = steam.active_user() {
            say!("   Steam logged in after {:.0}s", t.elapsed().as_secs_f32());
            return Ok(id);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(format!(
        "Steam started but didn't log in within {}s (it may want a password, Steam Guard code or an update): log in to Steam, then run this again",
        LOGIN_WAIT.as_secs()
    ))
}

/// While it lives: xtask's claim on the Steam client (`Half-Life/ts/xtask_steam.lock`, so two
/// runs don't start two games), and the user's friends status switched to invisible.
///
/// The status before is read from `localconfig.vdf` ([`Steam::persona_state`]); if that fails it
/// is put back to `online`. Dropping the guard puts it back (also when a check panics). For a
/// Ctrl-C or a kill, a watchdog shell in its own process group waits for xtask to exit and then
/// stops the game, removes the lock and puts the status back.
struct Invisible<'a> {
    steam: &'a Steam,
    lock: PathBuf,
    /// The status to put back; `None` when it was invisible already.
    restore: Option<&'static str>,
    watchdog: Option<Child>,
}

impl<'a> Invisible<'a> {
    /// Takes the lock. Fails when another live xtask holds it.
    fn lock(steam: &'a Steam) -> Result<Invisible<'a>, String> {
        let lock = steam.hl.join("ts").join("xtask_steam.lock");
        fs::create_dir_all(lock.parent().unwrap()).map_err(|e| e.to_string())?;
        for _ in 0..2 {
            match fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(Invisible { steam, lock, restore: None, watchdog: None });
                }
                Err(_) => {
                    let pid = fs::read_to_string(&lock).unwrap_or_default();
                    let pid = pid.trim();
                    if !pid.is_empty() && Path::new("/proc").join(pid).exists() {
                        return Err(format!("another `cargo xtask steam` (pid {pid}) is using the Steam client ({})", lock.display()));
                    }
                    // Left by a run that was killed.
                    let _ = fs::remove_file(&lock);
                }
            }
        }
        Err(format!("couldn't take {}", lock.display()))
    }

    /// Switches the friends status to invisible, remembering the one to put back.
    fn enter(&mut self, account: u32) {
        let before = self.steam.persona_state(account);
        let restore = match before.and_then(status_name) {
            Some("invisible") => {
                say!("   friends status: invisible already");
                None
            }
            Some(name) => Some(name),
            None => {
                say!("   friends status: couldn't read it from localconfig.vdf; it will be set back to online");
                Some("online")
            }
        };
        let Some(restore) = restore else { return };
        self.restore = Some(restore);
        // The watchdog first, so a Ctrl-C right after the switch still restores it.
        let script = r#"while kill -0 "$0" 2>/dev/null; do sleep 1; done
            if [ -n "$3" ]; then pkill -x hl_linux; fi
            rm -f "$1"
            steam "steam://friends/status/$2" >/dev/null 2>&1"#;
        let watchdog = {
            use std::os::unix::process::CommandExt;
            Command::new("sh")
                .args(["-c", script, &std::process::id().to_string()])
                .arg(&self.lock)
                .arg(restore)
                .arg("game")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .ok()
        };
        self.watchdog = watchdog;
        let ok = self.steam.set_status("invisible");
        say!(
            "   friends status: {} -> invisible{} (put back to {restore} afterwards)",
            before.and_then(status_name).unwrap_or("unknown"),
            if ok { "" } else { " (the steam:// request failed)" }
        );
    }
}

impl Drop for Invisible<'_> {
    fn drop(&mut self) {
        if let Some(mut w) = self.watchdog.take() {
            let _ = w.kill();
            let _ = w.wait();
        }
        if let Some(status) = self.restore.take() {
            let ok = self.steam.set_status(status);
            say!("== steam: friends status put back to {status}{}", if ok { "" } else { " (the steam:// request FAILED: set it by hand)" });
        }
        let _ = fs::remove_file(&self.lock);
    }
}

/// Stops the Steam Half-Life client and waits until it's gone.
fn stop_client() {
    let _ = Command::new("pkill").args(["-x", "hl_linux"]).status();
    let t = Instant::now();
    while running("hl_linux") && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    if running("hl_linux") {
        let _ = Command::new("pkill").args(["-KILL", "-x", "hl_linux"]).status();
    }
}

// ---------------------------------------------------------------------------------------------
// Scripts.

/// Steam's prologue for `play`'s scene helpers. HL25 runs in real time (xtask doesn't use its
/// `sys_timescale`), while `play`'s [`secs`] counts frames for FWGS at 2x game time. So the `w10` and
/// `w100` aliases wait twice as many frames: at `fps_max 100` a `secs(n)` is then `n` seconds here
/// too (the few plain `wait`s it adds are 10 ms short each).
fn prologue() -> String {
    let w10 = "wait;".repeat(20);
    format!("// Written by cargo xtask steam.\nalias w10 \"{w10}\"\nalias w100 \"{}\"\nfps_max 100\n", "w10;".repeat(10))
}

/// Listen-server settings, then the map (see `script_start` in main.rs for `lservercfgfile` and
/// `motdfile`).
fn listen_start(map: &str) -> String {
    prologue()
        + &format!(
            "developer 1\nsv_lan 1\nsv_cheats 1\nts_dev_scriptinput 1\nmaxplayers 8\nlservercfgfile \"\"\nmotdfile xtask_no_motd.txt\nmap {map}\n"
        )
}

/// `echo`ed by the client script, forwarded to the dedicated server's stdin by xtask (the HL25
/// client sometimes sends an `rcon` twice, which turns one `changelevel` into two).
fn srv(cmd: &str) -> String {
    format!("echo SRVCMD {cmd}\n")
}

/// A server command in a listen-server script.
fn local(cmd: &str) -> String {
    format!("{cmd}\n")
}

/// `ts_status` on the dedicated server, tagged in its log.
fn srv_status(tag: &str) -> String {
    srv(&format!("echo XTASK: status {tag}")) + &srv("ts_status")
}

/// The client's `ts_test_view` (third person, menu cursor, camera angles), tagged.
fn view(tag: &str) -> String {
    format!("echo XTASK: view {tag}\nts_test_view\n")
}

/// `changelevel ts_awaken` (`change`: the command, local or forwarded), wait for the new map and
/// its buy menu, buy and click to spawn again (TS resets the click timer in
/// `CHudSpectator::VidInit`), dump `status`, check the view (first person) and show the scoreboard.
fn changelevel_scene(change: &str, status: &str) -> String {
    let mut cfg = change.to_owned() + &secs(17.0) + &script_join_and_spawn();
    cfg += status;
    cfg += &view("changelevel");
    cfg += &shot("changelevel spawned");
    cfg + &format!("+showscores\n{}{}-showscores\n", secs(1.0), shot("changelevel scores"))
}

/// Server-set view angles while a menu owns the cursor (old #23: `CL_CreateMove` held the angles
/// and overwrote `svc_setangle`). The host is aimed along +x, then the MOTD is opened (`motd`, a
/// client command sent with `cmd`; `motdfile` back to TS's `motd.txt`) and the host is aimed along +y. The points are
/// far outside any map, so the yaw is 0 and then 90 wherever the host stands. `cmd` wraps a
/// server command.
fn menu_angles_scene(cmd: fn(&str) -> String) -> String {
    let mut cfg = cmd("ts_face 1 100000 0 0") + &secs(1.0) + &view("aim x");
    cfg += &cmd("motdfile motd.txt");
    // `cmd`: HL25's own `motd` command prints the client's copy instead of asking the server.
    cfg += &format!("{}cmd motd\n{}", secs(0.5), secs(2.0));
    cfg += &shot("motd");
    cfg += &cmd("ts_face 1 0 100000 0");
    cfg + &secs(1.0) + &view("aim y with the motd")
}

/// The `play` scenes on a Steam listen server on `ts_dojo`, in the order of [`Ctx::play`] (keep
/// them in step; the scene helpers are shared): join and wall shots, a stopped bot, lasers and
/// akimbo, power-up, grenade, kill, kick, death and respawn, four bots. Then, with the bots stopped, `changelevel ts_awaken` (buy and spawn again) and the
/// menu-angles scene, then `play`'s last scenes: the quota goes down, `addbot`, Escape.
fn listen_script() -> String {
    let mut cfg = listen_start("ts_dojo") + &script_join_and_spawn() + &status("spawned");
    cfg += &view("spawned");
    cfg += &script_wall_shots();

    cfg += &script_stopped_laser_bot();
    cfg += &script_lasers_and_akimbo();

    cfg += &format!("ts_tp 1 0 -672 60 90 0\n{}ts_tp 2 0 -480 60 270 0\n{}", secs(0.5), secs(1.5));
    cfg += &status("picked up");
    cfg += "+use\nwait;wait;wait\n-use\nwait;wait;wait\n+use\nwait;wait;wait\n-use\n";
    cfg += &secs(1.5);
    cfg += &status("power-up used");
    cfg += &shot("slow motion");
    cfg += &secs(3.0);

    cfg += &format!("ts_give 24\n{}weapon_24\n{}", secs(0.5), secs(1.5));
    cfg += &format!("ts_tp 1 -332 -420 60 90 0\n{}ts_face 1 -332 -164 60\n{}", secs(1.0), secs(0.5));
    cfg += "echo XTASK: throw\n+attack\nw10\n-attack\n";
    cfg += &secs(1.0);
    cfg += &format!("ts_tp 1 0 -1344 60 90 0\n{}", secs(5.0));

    cfg += &format!("weapon_13\n{}ts_tp 2 0 -1200 60 270 0\n{}", secs(1.5), secs(0.5));
    cfg += "echo XTASK: shoot\n+attack\n";
    for _ in 0..25 {
        cfg += &format!("ts_face 1 2\n{}", secs(0.2));
    }
    cfg += "-attack\n";
    cfg += &status("bot shot");

    cfg += &format!("kick \"Bot Neo\"\n{}", secs(4.0));

    cfg += &death_and_respawn(local, &status("host dead"), &status("host respawned"));

    cfg += &format!("kill\nts_bot_stop 0\n{}ts_bot_quota 4\n", secs(3.0));
    for i in 0..14 {
        cfg += &secs(5.0);
        cfg += &status(&format!("bots {i}"));
    }
    cfg += &shot("bots");
    // Not `play`'s gameplay-ruleset scene: `ts_gameplay` reloads the map with a server command,
    // which GoldSrc appends to the console buffer behind the running script, so the reload would
    // only happen after the script (`xtask ts21` covers the reload on the Steam engines).

    cfg += "ts_bot_stop 1\n";
    cfg += &changelevel_scene("echo XTASK: changelevel\nchangelevel ts_awaken\n", &status("changelevel"));
    cfg += &menu_angles_scene(local);

    cfg += "ts_bot_quota 0\n";
    cfg += &script_addbot();
    cfg += &script_escape();
    cfg + &format!("echo XTASK: done\nts_dev_touch {MARKER}\n")
}

/// `ts_hurt` kills the host, who clicks through the death cam and clicks to respawn; then the view
/// (first person again: old #23) and a screenshot (the world draws: old #62).
fn death_and_respawn(cmd: fn(&str) -> String, dead: &str, respawned: &str) -> String {
    let mut cfg = "echo XTASK: host killed\n".to_owned() + &cmd("ts_hurt 1 500");
    cfg += &secs(2.0);
    cfg += &click();
    cfg += &secs(1.0);
    cfg += dead;
    cfg += &secs(5.5);
    cfg += &click();
    cfg += &secs(2.0);
    cfg += respawned;
    cfg += &view("respawned");
    cfg + &shot("respawned")
}

/// The Steam client joins a dedicated server on `ts_dojo`: join, buy and spawn, a bot, chat, the
/// scoreboard, shoot the bot dead, death and respawn, the menu-angles scene (the server's MOTD is
/// up), then `changelevel ts_awaken`, buy and spawn again. Server commands go through the server's
/// stdin ([`srv`]).
fn dedicated_script(port: u16) -> String {
    let mut cfg = prologue() + "developer 1\nsv_cheats 1\nts_dev_scriptinput 1\n";
    cfg += &format!("connect 127.0.0.1:{port}\n{}", secs(7.0));
    cfg += &script_join_and_spawn();
    cfg += &srv_status("spawned");
    cfg += &view("spawned");
    cfg += &format!("{}{}", srv("ts_bot_quota 1"), secs(8.0));
    cfg += &format!("echo XTASK: chat\nsay xtask_chat_all\n{}say_team xtask_chat_team\n{}", secs(1.0), secs(1.0));
    cfg += &format!("+showscores\n{}{}-showscores\n", secs(1.0), shot("scores"));
    cfg += &format!("{}{}{}{}", srv("ts_tp 1 0 -1344 60 90 0"), secs(0.5), srv("ts_tp 2 0 -1200 60 270 0"), secs(1.0));
    cfg += &format!("weapon_13\n{}echo XTASK: shoot\n+attack\n", secs(1.0));
    for _ in 0..25 {
        cfg += &format!("{}{}", srv("ts_face 1 2"), secs(0.2));
    }
    cfg += "-attack\n";
    cfg += &srv_status("bot shot");
    cfg += &shot("bot shot");
    cfg += &death_and_respawn(srv, &srv_status("host dead"), &srv_status("host respawned"));
    cfg += &menu_angles_scene(srv);
    cfg += &changelevel_scene(&(srv("echo XTASK: changelevel") + &srv("changelevel ts_awaken")), &srv_status("changelevel"));
    cfg + &srv(&format!("ts_dev_touch {MARKER}"))
}

/// The `XTASK: <tag>` lines before each `screenshot` in a script, in order.
fn shot_tags(cfg: &str) -> Vec<String> {
    let lines: Vec<&str> = cfg.lines().collect();
    lines.windows(2).filter(|w| w[1] == "screenshot").filter_map(|w| w[0].strip_prefix("echo XTASK: ").map(str::to_owned)).collect()
}

// ---------------------------------------------------------------------------------------------
// Checks.

/// GoldSrc's tokenizer splits `XTASK:` into `XTASK` and `:`, and `echo` prints the tokens with
/// spaces between them (and one at the end): restore the markers the checks look for.
fn normalize(text: &str) -> String {
    text.replace('\r', "").replace("XTASK : ", "XTASK: ")
}

/// One `ts_test_view` answer.
#[derive(Debug, Clone, PartialEq)]
struct View {
    third: bool,
    menu: bool,
    angles: [f32; 3],
}

/// The `ts_test_view` answer after `XTASK: view <tag>`.
fn view_after(text: &str, tag: &str) -> Option<View> {
    let marker = format!("XTASK: view {tag}");
    let after = &text[text.lines().find(|l| l.trim_end().ends_with(&marker)).map(|l| l.as_ptr() as usize - text.as_ptr() as usize)?..];
    let line = after.lines().skip(1).find(|l| l.starts_with("third "))?;
    let word = |key: &str| line.split_whitespace().skip_while(|w| *w != key).nth(1);
    let angles = line.split("angles [").nth(1)?.split(']').next()?;
    let a: Vec<f32> = angles.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    Some(View { third: word("third")? == "true", menu: word("menu")? == "true", angles: [*a.first()?, *a.get(1)?, *a.get(2)?] })
}

/// `a` and `b` (degrees) are within `tol` of each other, modulo 360.
fn yaw_near(a: f32, b: f32, tol: f32) -> bool {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d) <= tol
}

/// The share of pixels of a screenshot (uncompressed 24/32-bit TGA, as GoldSrc writes them) that
/// aren't black: a channel above 4.
fn lit_share(tga: &Path) -> Option<f32> {
    let b = fs::read(tga).ok()?;
    if b.len() < 18 || b[2] != 2 || b[1] != 0 {
        return None;
    }
    let (w, h, bpp) = (u16::from_le_bytes([b[12], b[13]]) as usize, u16::from_le_bytes([b[14], b[15]]) as usize, b[16] as usize / 8);
    let start = 18 + b[0] as usize;
    let px = b.get(start..start + w * h * bpp)?;
    if bpp < 3 || w * h == 0 {
        return None;
    }
    let lit = px.chunks_exact(bpp).filter(|p| p[0].max(p[1]).max(p[2]) > 4).count();
    Some(lit as f32 / (w * h) as f32)
}

/// The world draws (old #62): at least half of the screenshot isn't black (above 4 of 255). Dark
/// maps and the MOTD's dimming pass: `ts_awaken`'s dim spawn is at 96%, 78% under the MOTD, while
/// a black world leaves only the HUD.
fn world_drawn(shots: &[(String, PathBuf, Option<f32>)], tag: &str) -> bool {
    shots.iter().find(|s| s.0 == tag).and_then(|s| s.2).is_some_and(|lit| lit > 0.5)
}

/// `[ts] ... spawned (#1)` lines (the host's spawns), in order.
fn host_spawns(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.contains("[ts] ") && l.contains("spawned (#1)")).collect()
}

/// A spawn line shows the AK47 (weapon 13) bought in the buy menu.
fn spawned_with_ak47(spawn_line: &str) -> bool {
    spawn_line.split_once(" with ").is_some_and(|(_, w)| w.contains("13"))
}

fn dump(dumps: &[(String, Vec<PlayerStatus>)], tag: &str) -> Vec<PlayerStatus> {
    dumps.iter().find(|d| d.0 == tag).map(|d| d.1.clone()).unwrap_or_default()
}

/// Player #1: the listen-server host, or the first client on a dedicated server.
fn host(dumps: &[(String, Vec<PlayerStatus>)], tag: &str) -> Option<PlayerStatus> {
    dump(dumps, tag).into_iter().find(|p| p.idx == 1)
}

/// The checks every Steam run makes: the client regressions of old #62 and #23. `stext` is the
/// server's log (the same text on a listen server).
fn steam_checks(text: &str, stext: &str, shots: &[(String, PathBuf, Option<f32>)]) -> Vec<(&'static str, bool)> {
    let dumps = status_dumps(stext);
    let after_change = stext.find("XTASK: changelevel").map_or("", |i| &stext[i..]);
    let v = |tag: &str| view_after(text, tag);
    let (aim_x, aim_y) = (v("aim x"), v("aim y with the motd"));
    say!(
        "   view: spawned {:?}; respawned {:?}; changelevel {:?}; aim x {:?}; aim y with the motd {:?}",
        v("spawned"),
        v("respawned"),
        v("changelevel"),
        aim_x,
        aim_y
    );
    for (tag, _, lit) in shots {
        if let Some(lit) = lit {
            say!("   screenshot {tag:?}: {:.0}% lit", lit * 100.0);
        }
    }
    vec![
        (
            "old #62: the world draws (screenshots after the respawn and on the new map aren't black)",
            world_drawn(shots, "respawned") && world_drawn(shots, "changelevel spawned"),
        ),
        (
            "old #23: first person after spawning and respawning (ts_test_view: third false)",
            ["spawned", "respawned", "changelevel"].iter().all(|t| v(t).is_some_and(|v| !v.third)),
        ),
        (
            "old #23: server-set view angles apply while the MOTD owns the cursor (yaw 0, then 90 with the menu up)",
            aim_x.is_some_and(|a| yaw_near(a.angles[1], 0.0, 5.0))
                && aim_y.is_some_and(|a| a.menu && yaw_near(a.angles[1], 90.0, 5.0) && a.angles[0].abs() < 5.0),
        ),
        ("old #23: changelevel to ts_awaken: bought an AK47 and clicked to spawn again", {
            after_change.contains("ts_awaken")
                && host_spawns(after_change).first().is_some_and(|l| spawned_with_ak47(l))
                && host(&dumps, "changelevel").is_some_and(|p| p.state == "alive")
        }),
    ]
}

/// [`Ctx::play`]'s checks on the Steam listen run (keep them in step). HL25 prints a kicked bot as
/// `- <name> has left the game` (the client's notice) rather than `<name> was kicked`.
fn listen_checks(run: &Run) -> Vec<(&'static str, bool)> {
    let text = &run.text;
    let dumps = status_dumps(text);
    let host = |tag: &str| host(&dumps, tag);
    let host_name = host("spawned").map(|p| p.name).unwrap_or_default();
    let spawns = host_spawns(text);
    let left = |name: &str| [format!("{name} was kicked"), format!("{name} has left the game")];
    let kick_at = text.find("XTASK: status bot shot");
    let after_kick = |needle: &str| kick_at.is_some_and(|k| text[k..].contains(needle));
    let kicked_neo = left("Bot Neo").iter().any(|l| after_kick(l));
    let kicks = text.matches("was kicked").count().max(text.matches("has left the game").count());

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

    let mut checks = vec![
        ("join: bought an AK47 and clicked to spawn", spawns.first().is_some_and(|l| spawned_with_ak47(l))),
        ("ts_bot_quota 1 added a bot", text.contains("\"ts_bot_quota\" is \"1\"") && text.contains("[ts] bots: added")),
        ("picked up the slow-motion power-up", host("picked up").is_some_and(|p| p.held == "0x1")),
        ("+use double tap: host at 0.6, the bot in view at 0.35", {
            let d = dump(&dumps, "power-up used");
            let host_ok = d.iter().any(|p| p.idx == 1 && p.active == "0x1" && (p.slow_target - 0.6).abs() < 0.01);
            host_ok && d.iter().any(|p| p.idx != 1 && (p.slow_target - 0.35).abs() < 0.01)
        }),
        ("the thrown grenade broke a func_breakable", text.contains(&format!("broken by {host_name} (grenade"))),
        ("the host shot a bot dead", text.contains(&format!("{host_name} killed Bot ")) && host("bot shot").is_some_and(|p| p.frags >= 1)),
        ("kicking the dead bot: no crash, the quota replaced it", kicked_neo && after_kick("[ts] bots: added") && !run.exited),
        ("the host died and clicked to respawn", {
            host("host dead").is_some_and(|p| p.deaths >= 1 && p.state != "alive")
                && spawns.len() >= 2
                && host("host respawned").is_some_and(|p| p.state == "alive")
        }),
        ("four bots moved around and killed each other", {
            last_bots >= 4 && moved.len() >= 2 && moving >= 2 && moving >= moved.len() - 1 && bot_kills >= 2
        }),
        ("ts_bot_quota 0 kicked the bots", kicks >= 4),
    ];
    checks.splice(1..1, wall_checks(text));
    let at = checks.iter().position(|c| c.0 == "ts_bot_quota 1 added a bot").map_or(checks.len(), |i| i + 1);
    checks.splice(at..at, laser_checks(text).into_iter().chain(akimbo_checks(text)));
    checks.extend(addbot_checks(text, &dumps));
    checks.extend(escape_checks(text));
    checks
}

fn is_screenshot(p: &Path) -> bool {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.len() == "HalfLife00.tga".len()
        && name.starts_with("HalfLife")
        && name.ends_with(".tga")
        && name[8..10].bytes().all(|b| b.is_ascii_digit())
}

// ---------------------------------------------------------------------------------------------
// The command.

/// The modes on the command line: `listen` and Linux GoldSrc server roots.
fn parse_modes(args: &[String]) -> Result<Vec<(String, Option<Root>)>, String> {
    let args: Vec<String> = if args.is_empty() { vec!["listen".into()] } else { args.to_vec() };
    args.into_iter()
        .map(|m| {
            if m == "listen" {
                return Ok((m, None));
            }
            let r = Root::all()
                .into_iter()
                .find(|r| r.name == m)
                .ok_or_else(|| format!("unknown mode or root {m:?}: use `listen` or a root from `cargo xtask roots`"))?;
            if r.engine == Engine::Fwgs {
                return Err(format!(
                    "{m}: the Steam client can't join an FWGS server (FWGS servers speak protocol 49, the Steam client 48)"
                ));
            }
            if r.os != Os::Linux {
                return Err(format!("{m}: only the Linux dedicated servers are supported (xtask drives the server console through stdin)"));
            }
            if !r.exists() {
                return Err(format!("{m}: {} missing", r.path.display()));
            }
            Ok((m, Some(r)))
        })
        .collect()
}

/// For `xtask all`: the Steam step runs only when Steam is running and logged in, and no other
/// Half-Life client or `xtask steam` is using it; otherwise it's skipped with a note (`None`).
/// It runs `listen` and, when the root exists, `hlds25-linux`.
pub fn all_step(ctx: &Ctx) -> Option<bool> {
    let skip = |why: &str| {
        say!("== steam: skipped ({why}; see docs/validation.md §5)");
        None
    };
    let Ok(steam) = Steam::locate() else { return skip("no Steam Half-Life install") };
    if !running("steam") || steam.active_user().is_none() {
        return skip("Steam isn't running and logged in");
    }
    if running("hl_linux") {
        return skip("a Half-Life client is already running");
    }
    let mut modes = vec!["listen".to_owned()];
    if Root::all().iter().any(|r| r.name == "hlds25-linux" && r.exists()) {
        modes.push("hlds25-linux".into());
    }
    Some(ctx.steam(&modes))
}

impl Ctx {
    /// `cargo xtask steam [listen|<server-root>...]`.
    pub fn steam(&self, args: &[String]) -> bool {
        let fail = |e: String| -> ! {
            eprintln!("steam: {e}");
            exit(1)
        };
        let modes = parse_modes(args).unwrap_or_else(|e| {
            eprintln!("steam: {e}");
            exit(2)
        });
        let steam = Steam::locate().unwrap_or_else(|e| fail(e));
        let mut session = Invisible::lock(&steam).unwrap_or_else(|e| fail(e));
        if running("hl_linux") {
            fail("a Half-Life client (hl_linux) is already running; only one can run at a time".into());
        }
        let account = match ensure_logged_in(&steam, true, &self.repo.join("target/steam-launch.log")) {
            Ok(id) => id,
            Err(e) => {
                drop(session);
                fail(e)
            }
        };
        say!("== steam: logged in (account {account}), Half-Life at {}", steam.hl.display());

        let t = Target::LINUX_I686;
        let gamedir = steam.hl.join("ts");
        self.build(&[t]);
        crate::link_ts_content(&crate::ts_dir(&self.repo), &gamedir);
        crate::copy_dir(&self.repo.join("game/ts"), &gamedir);
        self.install_libs(&t, &gamedir);
        say!("== steam: installed into {}", gamedir.display());

        session.enter(account);
        let mut results = Vec::new();
        for (i, (name, root)) in modes.iter().enumerate() {
            let ok = match root {
                None => self.steam_listen(&steam),
                Some(r) => self.steam_dedicated(&steam, r, DEDICATED_PORT + i as u16),
            };
            results.push((name.clone(), ok));
            if let Some(r) = root {
                results.push((format!("{name} 2.1"), self.steam_ts21(&steam, r, TS21_PORT + i as u16)));
            }
        }
        drop(session);
        let failed: Vec<&String> = results.iter().filter(|r| !r.1).map(|r| &r.0).collect();
        say!("== steam: {}", if failed.is_empty() { "all passed".to_owned() } else { format!("FAILED {failed:?}") });
        failed.is_empty()
    }

    /// Runs the Steam client with `cfg` (written as `ts/xtask_steam.cfg`) until `marker` exists
    /// plus `linger`, or `limit`. Every complete line of the client's console log goes to
    /// `on_line` as it's written. Screenshots are converted to `target/<tag>-N-<shot>.png`; each
    /// comes with its tag from the script and the share of lit pixels.
    #[allow(clippy::too_many_arguments)] // one call per mode; a struct would only rename them
    fn steam_client(
        &self,
        steam: &Steam,
        tag: &str,
        cfg: &str,
        marker: &Path,
        args: &[&str],
        mut on_line: impl FnMut(&str),
        linger: Duration,
        limit: Duration,
    ) -> (Run, Vec<(String, PathBuf, Option<f32>)>) {
        let gamedir = steam.hl.join("ts");
        // No `-` in the name: the engine splits `+exec xtask_steam-listen.cfg` at the dash.
        let cfg_name = "xtask_steam.cfg";
        let cfg_path = gamedir.join(cfg_name);
        fs::write(&cfg_path, cfg).unwrap();
        let _ = fs::remove_file(marker);
        // -condebug writes to the working directory (Half-Life/); older builds used ts/.
        let consoles = [steam.hl.join("qconsole.log"), gamedir.join("qconsole.log")];
        for c in &consoles {
            let _ = fs::remove_file(c);
        }
        let tgas = |dir: &Path| -> Vec<PathBuf> {
            let mut v: Vec<PathBuf> =
                fs::read_dir(dir).map(|d| d.flatten().map(|e| e.path()).filter(|p| is_screenshot(p)).collect()).unwrap_or_default();
            v.sort();
            v
        };
        // Old screenshots would shift the numbering of the new ones: move them out of the way.
        for old in tgas(&gamedir) {
            let _ = fs::remove_file(old);
        }

        let out = fs::File::create(self.repo.join("target").join(format!("{tag}-stdout.log"))).unwrap();
        let mut child = {
            use std::os::unix::process::CommandExt;
            Command::new(steam.root.join("ubuntu12_32/steam-runtime/run.sh"))
                .current_dir(&steam.hl)
                .env("SteamAppId", HALF_LIFE_APP)
                .env("SteamGameId", HALF_LIFE_APP)
                .arg("./hl.sh")
                .args(["-game", "ts", "-windowed", "-width", "1280", "-height", "800", "-condebug"])
                .args(args)
                .args(["+exec", cfg_name])
                .stdin(Stdio::null())
                .stdout(out.try_clone().unwrap())
                .stderr(out)
                // Its own process group, so a Ctrl-C meant for xtask doesn't reach the game first
                // (the watchdog of `Invisible` stops it).
                .process_group(0)
                .spawn()
                .expect("start the Steam runtime")
        };
        let t = Instant::now();
        let (mut exited, mut done_at) = (false, None);
        let (mut pos, mut partial, mut console) = (0u64, String::new(), None::<PathBuf>);
        let mut follow = |console: &mut Option<PathBuf>, partial: &mut String, pos: &mut u64| {
            if console.is_none() {
                *console = consoles.iter().find(|c| c.exists()).cloned();
            }
            let Some(Ok(mut f)) = console.as_ref().map(fs::File::open) else { return };
            if f.seek(SeekFrom::Start(*pos)).is_err() {
                return;
            }
            let mut buf = Vec::new();
            if let Ok(n) = f.read_to_end(&mut buf) {
                *pos += n as u64;
                partial.push_str(&String::from_utf8_lossy(&buf));
                while let Some(nl) = partial.find('\n') {
                    let line: String = partial.drain(..=nl).collect();
                    on_line(&normalize(line.trim_end()));
                }
            }
        };
        while t.elapsed() < limit {
            follow(&mut console, &mut partial, &mut pos);
            if child.try_wait().ok().flatten().is_some() && !running("hl_linux") {
                exited = true;
                break;
            }
            if done_at.is_none() && marker.exists() {
                done_at = Some(Instant::now());
            }
            if done_at.is_some_and(|d| d.elapsed() >= linger) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        stop_client();
        let _ = child.kill();
        let _ = child.wait();
        follow(&mut console, &mut partial, &mut pos);
        let _ = fs::remove_file(&cfg_path);
        let _ = fs::remove_file(marker);
        say!("   script ran {:.0}s (real time)", done_at.map_or(t.elapsed(), |d| d - t).as_secs_f32());
        if done_at.is_none() && !exited {
            say!("   the script didn't finish within {}s", limit.as_secs());
        }
        if exited {
            say!("   the game exited on its own");
        }

        let text = normalize(&String::from_utf8_lossy(&console.as_ref().and_then(|c| fs::read(c).ok()).unwrap_or_default()));
        let log = self.repo.join("target").join(format!("{tag}.log"));
        let _ = fs::write(&log, &text);
        let tags = shot_tags(cfg);
        let new = tgas(&gamedir);
        if new.len() != tags.len() {
            say!("   {} screenshots for {} in the script: they can't be matched to their scenes", new.len(), tags.len());
        }
        let mut shots = Vec::new();
        for (i, p) in new.iter().enumerate() {
            let shot_tag = if new.len() == tags.len() { tags[i].clone() } else { format!("shot {}", i + 1) };
            let lit = lit_share(p);
            let slug: String = shot_tag.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            let dest = self.keep_screenshot(p, &format!("{tag}-{:02}-{slug}", i + 1));
            shots.push((shot_tag, dest, lit));
        }
        let paths = shots.iter().map(|s| s.1.clone()).collect();
        (Run { text, shots: paths, exited, done: done_at.is_some() }, shots)
    }

    /// Moves a screenshot out of the game folder into `target/<name>.png` (with ImageMagick) or
    /// `target/<name>.tga`.
    fn keep_screenshot(&self, tga: &Path, name: &str) -> PathBuf {
        let png = self.repo.join("target").join(format!("{name}.png"));
        let _ = fs::remove_file(&png);
        let converted =
            Command::new("magick").arg(tga).arg(&png).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        let dest = if converted {
            png
        } else {
            let dest = png.with_extension("tga");
            let _ = fs::copy(tga, &dest);
            dest
        };
        let _ = fs::remove_file(tga);
        dest
    }

    fn steam_report(&self, shots: &[PathBuf], checks: &[(&str, bool)], logs: &[PathBuf]) -> bool {
        for p in shots {
            say!("   screenshot: {}", p.display());
        }
        let mut ok = true;
        for (what, pass) in checks {
            say!("   {} {what}", if *pass { "ok  " } else { "FAIL" });
            ok &= *pass;
        }
        let logs: Vec<String> = logs.iter().map(|l| l.display().to_string()).collect();
        say!("   {} (logs: {})", crate::pass(ok), logs.join(", "));
        ok
    }

    /// [`listen_script`] on a Steam listen server.
    fn steam_listen(&self, steam: &Steam) -> bool {
        say!("== steam listen: gameplay on a Steam HL25 listen server, ts_dojo then ts_awaken (port {LISTEN_PORT})");
        let cfg = listen_script();
        let marker = steam.hl.join("ts").join(MARKER);
        let port = LISTEN_PORT.to_string();
        // The engine's `exec config.cfg` and the game's `exec skill.cfg` (TS ships none, so
        // Half-Life's `valve/skill.cfg`: 393 `sk_*` lines) wait behind the script. Run after it,
        // with the host in the game, each `sk_*` (TS registers none, as the port doesn't) goes to
        // the server as a client command and comes back as an "Unknown command" message, and the
        // host's reliable channel overflows (`Host_EndGame`) before the quota's kicks run. An
        // empty `ts/skill.cfg` for the run keeps the fallback out.
        let skill = steam.hl.join("ts/skill.cfg");
        let own_skill =
            !skill.exists() && fs::write(&skill, "// Written by cargo xtask steam for its listen run; removed afterwards.\n").is_ok();
        let (run, shots) = self.steam_client(
            steam,
            "steam-listen",
            &cfg,
            &marker,
            &["-port", &port],
            |_| {},
            // The quota's kicks run after the script (one per 0.5 s).
            Duration::from_secs(8),
            Duration::from_secs(1200),
        );
        if own_skill {
            let _ = fs::remove_file(&skill);
        }
        let text = &run.text;
        for line in text.lines().filter(|l| {
            l.contains("XTASK:") && !l.contains("XTASK: status") && !l.contains("XTASK: view")
                || l.contains("spawned (#1)")
                || l.contains("func_breakable") && !l.contains("by Bot")
                || l.contains("kicked")
                || l.contains("has left the game")
        }) {
            say!("   | {}", line.trim_end());
        }
        let mut checks = listen_checks(&run);
        checks.extend(steam_checks(text, text, &shots));
        checks.push(("the script finished, no panic or crash", run.done && !run.exited && !crashed(text)));
        self.steam_report(&run.shots, &checks, &[self.repo.join("target/steam-listen.log")])
    }

    /// The Steam client joins `root`'s server in 2.1 mode (`xtask ts21`'s server config): the
    /// server must let the port's client in, and the client must be told ruleset 21. GoldSrc only
    /// puts the port's `_tsrs` key in the userinfo once the cvar is set, which FWGS (`xtask ts21`'s
    /// client) doesn't show.
    fn steam_ts21(&self, steam: &Steam, root: &Root, port: u16) -> bool {
        say!("== steam {}: Steam HL25 client joins a 2.1 server (port {port})", root.name);
        let Some(mut server) = self.ts21_client_server(root, port) else {
            say!("   FAIL: the server didn't answer");
            return false;
        };
        let mut stdin = server.stdin.take().unwrap();
        let cfg = prologue()
            + &format!("connect 127.0.0.1:{port}\n{}echo XTASK: in game\n", secs(12.0))
            + &srv(&format!("ts_dev_touch {MARKER}"));
        let marker = root.path.join("ts").join(MARKER);
        let (run, _) = self.steam_client(
            steam,
            &format!("steam-{}-ts21", root.name),
            &cfg,
            &marker,
            &[],
            |line| {
                if let Some(cmd) = line.split("SRVCMD ").nth(1).filter(|_| !line.contains("echo SRVCMD")) {
                    let _ = writeln!(stdin, "{}", cmd.trim());
                    let _ = stdin.flush();
                }
            },
            Duration::from_secs(1),
            Duration::from_secs(90),
        );
        drop(stdin);
        let _ = fs::remove_file(&marker);
        let stext = normalize(&self.ts21_client_stop(root, server));
        let server_log = self.repo.join("target").join(format!("steam-{}-ts21-server.log", root.name));
        let _ = fs::write(&server_log, &stext);
        let text = &run.text;
        let checks = [
            ("the server didn't refuse the port's client", !stext.contains("[ts] gameplay: refused")),
            (
                "the client: \"[ts] gameplay: the server runs ruleset 21\" (SrvSett 4)",
                text.contains("[ts] gameplay: the server runs ruleset 21"),
            ),
            ("the script finished, no panic or crash", run.done && !run.exited && !crashed(text) && !stext.contains("PANIC")),
        ];
        self.steam_report(&[], &checks, &[self.repo.join(format!("target/steam-{}-ts21.log", root.name)), server_log])
    }

    /// [`dedicated_script`]: the Steam client on `root`'s dedicated server.
    fn steam_dedicated(&self, steam: &Steam, root: &Root, port: u16) -> bool {
        self.install(root);
        say!("== steam {}: Steam HL25 client on a dedicated server, ts_dojo then ts_awaken (port {port})", root.name);
        // The per-map config: these come back after the changelevel. Logging (to the console
        // only) puts the chat lines in the server log. The strictest version settings (#300):
        // the Steam client's `_tsrs` must read as the server's own version at every connect.
        let server_cfg = root.path.join("ts/xtask_steam_server.cfg");
        fs::write(
            &server_cfg,
            format!(
                "// Written by cargo xtask steam.\n{EXEC_SERVER_CFG}sv_cheats 1\nts_bot_quota 0\nts_bot_stop 1\ndeveloper 1\nmp_logfile 0\nlog on\nsv_client_version exact\nsv_client_require_port 1\n"
            ),
        )
        .unwrap();
        let out_path = self.repo.join("target").join(format!("server-{}.log", root.name));
        let out = fs::File::create(&out_path).unwrap();
        let _ = fs::remove_file(root.log_path());
        let mut server = root
            .server_command_with("ts_dojo", port, "xtask_steam_server.cfg", 0)
            .stdin(Stdio::piped())
            .stdout(out.try_clone().unwrap())
            .stderr(out)
            .spawn()
            .expect("start server");
        let mut stdin = server.stdin.take().unwrap();
        let t = Instant::now();
        let mut up = false;
        while !up && t.elapsed() < Duration::from_secs(60) && server.try_wait().ok().flatten().is_none() {
            std::thread::sleep(Duration::from_millis(500));
            up = a2s::info("127.0.0.1", port).is_ok();
        }
        let stop_server = |mut server: Child| {
            let t = Instant::now();
            while server.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(200));
            }
            let _ = server.kill();
            let _ = server.wait();
            let _ = fs::remove_file(&server_cfg);
        };
        if !up {
            say!("   FAIL: the server didn't answer within 60s (log: {})", out_path.display());
            drop(stdin);
            stop_server(server);
            return false;
        }
        say!("   server up after {:.1}s", t.elapsed().as_secs_f32());

        let cfg = dedicated_script(port);
        let marker = root.path.join("ts").join(MARKER);
        let mut forwarded = 0;
        let (run, shots) = self.steam_client(
            steam,
            &format!("steam-{}", root.name),
            &cfg,
            &marker,
            &[],
            |line| {
                if let Some(cmd) = line.split("SRVCMD ").nth(1).filter(|_| !line.contains("echo SRVCMD")) {
                    forwarded += 1;
                    let _ = writeln!(stdin, "{}", cmd.trim());
                    let _ = stdin.flush();
                }
            },
            Duration::from_secs(2),
            Duration::from_secs(600),
        );
        let server_alive = server.try_wait().ok().flatten().is_none();
        let _ = writeln!(stdin, "quit");
        drop(stdin);
        stop_server(server);
        let _ = fs::remove_file(&marker);
        say!("   {forwarded} server commands forwarded");

        let stext = normalize(&String::from_utf8_lossy(&fs::read(&out_path).unwrap_or_default()));
        let server_log = self.repo.join("target").join(format!("steam-{}-server.log", root.name));
        let _ = fs::write(&server_log, &stext);
        for line in stext.lines().filter(|l| {
            l.contains("XTASK:") && !l.contains("XTASK: status")
                || l.contains("spawned (#1)")
                || l.contains(" killed ")
                || l.contains("xtask_chat")
                || l.contains("bots: added")
        }) {
            say!("   | {}", line.trim_end());
        }
        let dumps = status_dumps(&stext);
        let host = |tag: &str| host(&dumps, tag);
        let host_name = host("spawned").map(|p| p.name).unwrap_or_default();
        let spawns = host_spawns(&stext);
        let ctext = &run.text;
        let mut checks = vec![
            ("join: bought an AK47 and clicked to spawn", spawns.first().is_some_and(|l| spawned_with_ak47(l))),
            ("ts_bot_quota 1 added a bot", stext.contains("[ts] bots: added")),
            ("sv_client_version exact let the Steam client in", !stext.contains("[ts] version: refused")),
            (
                "chat: say and say_team reached the server and came back to the client",
                stext.contains("say \"xtask_chat_all\"")
                    && stext.contains("say_team \"xtask_chat_team\"")
                    && ctext.contains("xtask_chat_all")
                    && ctext.contains("xtask_chat_team"),
            ),
            // The kill notice is the client's (`DeathMsg`), printed to its console.
            (
                "the host shot a bot dead",
                ctext.contains(&format!("{host_name} killed Bot ")) && host("bot shot").is_some_and(|p| p.frags >= 1),
            ),
            ("the host died and clicked to respawn", {
                host("host dead").is_some_and(|p| p.deaths >= 1 && p.state != "alive")
                    && spawns.len() >= 2
                    && host("host respawned").is_some_and(|p| p.state == "alive")
            }),
        ];
        checks.extend(steam_checks(ctext, &stext, &shots));
        checks.push((
            "the script finished, no panic or crash (client and server)",
            run.done && !run.exited && server_alive && !crashed(ctext) && !crashed(&stext),
        ));
        self.steam_report(&run.shots, &checks, &[self.repo.join("target").join(format!("steam-{}.log", root.name)), server_log])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARYFOLDERS: &str = r#""libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"label"		""
		"apps"
		{
			"228980"		"1"
		}
	}
	"1"
	{
		"path"		"/mnt/games/Steam Library"
		"apps"
		{
			"70"		"667402149"  // Half-Life
		}
	}
}
"#;

    #[test]
    fn parses_libraryfolders() {
        assert_eq!(
            library_folders(LIBRARYFOLDERS, "70"),
            [PathBuf::from("/mnt/games/Steam Library"), PathBuf::from("/home/u/.local/share/Steam")]
        );
        let old = "\"LibraryFolders\"\n{\n\t\"TimeNextStatsReport\"\t\"1\"\n\t\"1\"\t\"/data/steam\"\n}\n";
        assert_eq!(library_folders(old, "70"), [PathBuf::from("/data/steam")]);
        assert_eq!(library_folders("", "70"), Vec::<PathBuf>::new());
    }

    #[test]
    fn parses_vdf_escapes_and_nesting() {
        let v = parse_vdf(r#""a" { "b" "x\"y\\z" "c" { "d" "1" } } "e" "2""#);
        assert_eq!(v.get("a").and_then(|a| a.get("b")), Some(&Vdf::Str("x\"y\\z".into())));
        assert_eq!(v.path("a/c/D"), Some(&Vdf::Str("1".into())));
        assert_eq!(v.find("d").and_then(Vdf::str), Some("1"));
        assert_eq!(v.get("e"), Some(&Vdf::Str("2".into())));
    }

    #[test]
    fn reads_the_persona_state() {
        // As in localconfig.vdf (the JSON is a VDF string with escaped quotes).
        let cfg = parse_vdf(
            r#""UserLocalConfigStore" { "WebStorage" { "FriendStoreLocalPrefs_11510518" "{\"ePersonaState\":3,\"strNonFriendsAllowedToMsg\":\"\"}" } }"#,
        );
        let json = cfg.find("FriendStoreLocalPrefs_11510518").and_then(Vdf::str).unwrap();
        assert_eq!(persona_from_prefs(json), Some(3));
        assert_eq!(status_name(3), Some("away"));
        assert_eq!(status_name(7), Some("invisible"));
        assert_eq!(persona_from_prefs("{}"), None);
    }

    #[test]
    fn reads_the_login_from_the_connection_log() {
        let on = "[2026-10-15 11:50:17] [Logging On, 4, 7] [U:1:11510518] RecvMsgClientLogOnResponse() : [U:1:11510518] 'OK'\n\
                  [2026-10-15 11:50:17] [Logged On, 4, 7] [U:1:11510518] RecvMsgClientLogOnResponse() : processing complete\n\
                  [2026-10-15 11:50:17] CClientJobGetClientUpdateHosts: cached version not expired\n";
        assert_eq!(logged_on_account(on), Some(11510518));
        let off = format!("{on}[2026-10-15 12:00:38] [Logged Off, 0, 0] [U:1:11510518] Log session ended\n");
        assert_eq!(logged_on_account(&off), None);
        assert_eq!(logged_on_account("[2026-10-15 11:50:11] [Logged Off, 0, 0] [U:1:0] CCMInterface::SetSteamID( [U:1:0] )\n"), None);
    }

    #[test]
    fn normalizes_goldsrc_echo() {
        assert_eq!(
            normalize("XTASK : status spawned \r\nSRVCMD ts_tp 1 0 -1344 60 90 0 "),
            "XTASK: status spawned \nSRVCMD ts_tp 1 0 -1344 60 90 0 "
        );
        assert!(is_screenshot(Path::new("/x/HalfLife07.tga")) && !is_screenshot(Path::new("/x/ts_logo.tga")));
    }

    #[test]
    fn reads_ts_test_view() {
        let log = "XTASK: view respawned \nthird false menu true cam_back 0.0 cam_up 0.0 model_z 0.0 amt 255 origin [1.0, 2.0, 3.0] eye [1.0, 2.0, 30.0] angles [0.5, 89.5, 0.0]\nts 1.000/1.000 fov 90.0/90.0\n";
        assert_eq!(view_after(log, "respawned"), Some(View { third: false, menu: true, angles: [0.5, 89.5, 0.0] }));
        assert_eq!(view_after(log, "spawned"), None);
        assert!(yaw_near(359.0, 0.0, 5.0) && yaw_near(-2.0, 0.0, 5.0) && !yaw_near(90.0, 0.0, 5.0));
    }

    #[test]
    fn scripts_fit_and_tag_their_screenshots() {
        let listen = listen_script();
        let tags = shot_tags(&listen);
        for t in ["respawned", "changelevel spawned", "motd"] {
            assert!(tags.iter().any(|x| x == t), "{t} in {tags:?}");
        }
        let dedicated = dedicated_script(27160);
        assert!(shot_tags(&dedicated).iter().any(|x| x == "respawned"));
        // GoldSrc's command buffer is 16 KB on the Steam engine; `exec` inserts the whole file, but
        // the waits are aliases, so the scripts stay small.
        for cfg in [&listen, &dedicated] {
            assert!(cfg.len() < 16 * 1024, "{} bytes", cfg.len());
            assert!(cfg.contains(&format!("ts_dev_touch {MARKER}")));
        }
        // The dedicated script marks every server command; the client runs none of them itself.
        assert!(!dedicated.lines().any(|l| l.starts_with("ts_tp") || l.starts_with("ts_hurt") || l.starts_with("changelevel")));
    }

    #[test]
    fn lit_share_of_a_tga() {
        let dir = std::env::temp_dir().join(format!("xtask-steam-tga-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.tga");
        let mut b = vec![0u8; 18];
        b[2] = 2;
        b[12] = 2;
        b[14] = 1;
        b[16] = 24;
        b.extend([0, 0, 0, 200, 200, 200]);
        fs::write(&p, b).unwrap();
        assert_eq!(lit_share(&p), Some(0.5));
        let _ = fs::remove_dir_all(dir);
    }
}
