//! `ts-launcher-fake`: what the port's relaunch e2e test (`cargo xtask relaunch`) needs of
//! tsmod-launcher, speaking the protocol of docs/implementation/client-plugins.md §13.5:
//!
//! ```text
//! ts-launcher-fake serve --endpoint <socket path | \\.\pipe\name>
//!     the running launcher: answers each one-line JSON request, then waits for its pid and
//!     starts the game with +connect / +tsrs_reconnect <address> password
//! ts-launcher-fake relaunch --instance ID --wait-pid PID --connect ADDR [--password-required] ...
//!     the launcher started by the game: the same, from the arguments
//! ```
//!
//! Everything it receives goes to the log named in `ts-launcher-fake.cfg` next to the
//! executable: each request line (`ipc ...`), each argument (`arg ...`), its environment
//! (`env K=V`, relaunch only: what the game passed on), and what it started. The test reads the
//! log, for one thing to check that no password is in it. The config's lines:
//!
//! ```text
//! log=<file>          appended to
//! answer=ok           ok | error | silent (accept and never answer)
//! game=<program>      the game to start (the engine binary)
//! cwd=<dir>           where
//! arg=<word>          an argument before the connect command (repeat)
//! post=<word>         an argument after it (repeat)
//! ```

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, exit};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Config {
    log: Option<PathBuf>,
    answer: String,
    game: Option<PathBuf>,
    cwd: Option<PathBuf>,
    pre: Vec<String>,
    post: Vec<String>,
}

fn config() -> Config {
    let path = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("ts-launcher-fake.cfg")));
    let text = path.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let mut c = Config { answer: "ok".into(), ..Config::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        match k.trim() {
            "log" => c.log = Some(v.into()),
            "answer" => c.answer = v.trim().into(),
            "game" => c.game = Some(v.into()),
            "cwd" => c.cwd = Some(v.into()),
            "arg" => c.pre.push(v.into()),
            "post" => c.post.push(v.into()),
            _ => {}
        }
    }
    c
}

fn log(c: &Config, line: &str) {
    let line = line.replace(['\n', '\r'], " ");
    eprintln!("ts-launcher-fake: {line}");
    if let Some(p) = &c.log
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// What a request asks for, from the JSON line or the arguments.
struct Relaunch {
    pid: u32,
    connect: String,
    password_required: bool,
}

/// The value after `"key":` in a flat JSON object (a string's contents, or the bare literal).
fn json_field(line: &str, key: &str) -> Option<String> {
    let i = line.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = line[i..].trim_start().strip_prefix(':')?.trim_start();
    if let Some(s) = rest.strip_prefix('"') {
        Some(s[..s.find('"')?].to_owned())
    } else {
        Some(rest.split([',', '}']).next()?.trim().to_owned())
    }
}

fn from_json(line: &str) -> Option<Relaunch> {
    (json_field(line, "op")? == "relaunch").then_some(())?;
    Some(Relaunch {
        pid: json_field(line, "pid")?.parse().ok()?,
        connect: json_field(line, "connect")?,
        password_required: json_field(line, "password_required").as_deref() == Some("true"),
    })
}

fn from_args(args: &[String]) -> Option<Relaunch> {
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    Some(Relaunch {
        pid: opt("--wait-pid")?.parse().ok()?,
        connect: opt("--connect")?,
        password_required: args.iter().any(|a| a == "--password-required"),
    })
}

/// Waits for the game to exit (up to 30 s, as the launcher does), then starts it again.
fn relaunch(c: &Config, r: &Relaunch) {
    let t = Instant::now();
    while alive(r.pid) {
        if t.elapsed() > Duration::from_secs(30) {
            log(c, &format!("pid {} still running after 30 s; not relaunching", r.pid));
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    log(c, &format!("pid {} exited after {:.1} s", r.pid, t.elapsed().as_secs_f32()));
    let Some(game) = &c.game else {
        log(c, "no game= in the config; not relaunching");
        return;
    };
    let mut args = c.pre.clone();
    if r.password_required {
        args.extend(["+tsrs_reconnect".into(), r.connect.clone(), "password".into()]);
    } else {
        args.extend(["+connect".into(), r.connect.clone()]);
    }
    args.extend(c.post.iter().cloned());
    let mut cmd = Command::new(game);
    cmd.args(&args);
    if let Some(d) = &c.cwd {
        cmd.current_dir(d);
    }
    log(c, &format!("start {} {}", game.display(), args.join(" ")));
    match cmd.spawn() {
        Ok(child) => log(c, &format!("started pid {}", child.id())),
        Err(e) => log(c, &format!("start failed: {e}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let c = config();
    match args.first().map(String::as_str) {
        Some("relaunch") => {
            for a in &args {
                log(&c, &format!("arg {a}"));
            }
            let mut env: Vec<(String, String)> =
                std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
            env.sort();
            for (k, v) in env {
                log(&c, &format!("env {k}={v}"));
            }
            match from_args(&args) {
                Some(r) => relaunch(&c, &r),
                None => log(&c, "bad relaunch arguments"),
            }
        }
        Some("serve") => {
            let Some(ep) = args.iter().position(|a| a == "--endpoint").and_then(|i| args.get(i + 1)) else {
                eprintln!("usage: ts-launcher-fake serve --endpoint <path|pipe>");
                exit(2)
            };
            serve(&c, ep);
        }
        _ => {
            eprintln!("usage: ts-launcher-fake serve --endpoint <path|pipe> | relaunch ...");
            exit(2)
        }
    }
}

/// Answers one request on `stream`; returns the request to carry out.
fn answer<S: std::io::Read + Write>(c: &Config, stream: S) -> Option<Relaunch> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return None;
    }
    log(c, &format!("ipc {}", line.trim_end()));
    let req = from_json(&line);
    let reply = match (c.answer.as_str(), &req) {
        ("silent", _) => {
            log(c, "not answering (answer=silent)");
            std::thread::sleep(Duration::from_secs(5));
            return None;
        }
        ("error", _) | (_, None) => "{\"ok\":false,\"error\":\"refused by the test\"}\n",
        _ => "{\"ok\":true}\n",
    };
    let _ = reader.get_mut().write_all(reply.as_bytes());
    let _ = reader.get_mut().flush();
    log(c, &format!("answered {}", reply.trim_end()));
    if reply.contains("true") { req } else { None }
}

#[cfg(unix)]
fn serve(c: &Config, ep: &str) {
    let _ = std::fs::remove_file(ep);
    let l = std::os::unix::net::UnixListener::bind(ep).unwrap_or_else(|e| {
        eprintln!("{ep}: {e}");
        exit(1)
    });
    log(c, &format!("listening on {ep}"));
    for s in l.incoming().flatten() {
        if let Some(r) = answer(c, s) {
            relaunch(c, &r);
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // A zombie (exited, not yet reaped by its parent) counts as gone.
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(s) => s.rsplit_once(')').is_none_or(|(_, rest)| !rest.trim_start().starts_with(['Z', 'X'])),
        Err(_) => false,
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    pub type Handle = *mut c_void;
    pub const INVALID: Handle = -1isize as Handle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn CreateNamedPipeW(
            name: *const u16,
            open: u32,
            mode: u32,
            max: u32,
            out: u32,
            inp: u32,
            timeout: u32,
            sa: *mut c_void,
        ) -> Handle;
        pub fn ConnectNamedPipe(pipe: Handle, ov: *mut c_void) -> i32;
        pub fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        pub fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
        pub fn CloseHandle(h: Handle) -> i32;
        pub fn GetLastError() -> u32;
    }
}

#[cfg(windows)]
fn serve(c: &Config, ep: &str) {
    use std::os::windows::io::FromRawHandle;
    let name: Vec<u16> = ep.encode_utf16().chain([0]).collect();
    log(c, &format!("listening on {ep}"));
    loop {
        const PIPE_ACCESS_DUPLEX: u32 = 3;
        const ERROR_PIPE_CONNECTED: u32 = 535;
        // SAFETY: `name` is a NUL-terminated UTF-16 string; the other arguments are plain values
        // (byte mode, blocking, one instance, 4 KB buffers, default security).
        let h = unsafe { win::CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, 0, 1, 4096, 4096, 0, std::ptr::null_mut()) };
        if h == win::INVALID {
            eprintln!("CreateNamedPipeW {ep} failed");
            exit(1);
        }
        // SAFETY: `h` is the pipe handle just created; no OVERLAPPED (blocking).
        let connected =
            unsafe { win::ConnectNamedPipe(h, std::ptr::null_mut()) } != 0 || unsafe { win::GetLastError() } == ERROR_PIPE_CONNECTED;
        // SAFETY: `h` is an open handle this function owns; the File closes it when dropped.
        let f = unsafe { std::fs::File::from_raw_handle(h) };
        if connected && let Some(r) = answer(c, f) {
            relaunch(c, &r);
        }
    }
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 0x102;
    // SAFETY: plain values; a null handle means there is no such process (or it is gone).
    let h = unsafe { win::OpenProcess(SYNCHRONIZE, 0, pid) };
    if h.is_null() {
        return false;
    }
    // SAFETY: `h` is the process handle just opened, closed right after.
    let running = unsafe { win::WaitForSingleObject(h, 0) } == WAIT_TIMEOUT;
    // SAFETY: as above.
    unsafe { win::CloseHandle(h) };
    running
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_requests() {
        let r = from_json(r#"{"op":"relaunch","v":1,"pid":41733,"connect":"203.0.113.5:27015","password_required":true}"#).unwrap();
        assert_eq!((r.pid, r.connect.as_str(), r.password_required), (41733, "203.0.113.5:27015", true));
        assert!(from_json(r#"{"op":"other","pid":1,"connect":"a:1"}"#).is_none());
        let a: Vec<String> = ["relaunch", "--instance", "e2e", "--wait-pid", "7", "--connect", "a:1"].map(String::from).to_vec();
        let r = from_args(&a).unwrap();
        assert_eq!((r.pid, r.connect.as_str(), r.password_required), (7, "a:1", false));
    }
}
