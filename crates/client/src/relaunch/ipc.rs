//! The launcher IPC request (docs/implementation/client-plugins.md §13.5 case 1): one JSON line
//! to the endpoint in `TSMOD_LAUNCHER_IPC` (a Unix socket path on Linux, a `\\.\pipe\` name on
//! Windows), one JSON line back. Protocol version 1:
//!
//! ```json
//! {"op":"relaunch","v":1,"instance":"ab12","pid":41733,"connect":"203.0.113.5:27015",
//!  "password_required":true,"reason":"port","port_version":">=3.0.8, <3.1",
//!  "url":"https://tsmod.org","server_name":"Bob's TS","client_version":"3.0.8"}
//! ```
//!
//! Answer `{"ok":true}` (the client quits) or `{"ok":false,"error":"..."}` (the client falls back
//! to starting `ts-launcher relaunch`), within [`TIMEOUT`].

use super::plan::{Reason, Request};
use std::time::Duration;

/// How long the client waits for the launcher's answer before falling back.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// The protocol version, the request's `v`.
pub const VERSION: u32 = 1;

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The request line (with its `\n`). Optional fields are left out when they don't apply.
pub fn request_line(req: &Request, instance: Option<&str>, pid: u32) -> String {
    let mut f: Vec<(&str, String)> = vec![("op", json_str("relaunch")), ("v", VERSION.to_string())];
    if let Some(i) = instance {
        f.push(("instance", json_str(i)));
    }
    f.push(("pid", pid.to_string()));
    f.push(("connect", json_str(&req.connect)));
    f.push(("password_required", req.password_required.to_string()));
    f.push(("reason", json_str(req.reason.name())));
    match &req.reason {
        Reason::Restart => {}
        Reason::Port { requirement, url } => {
            f.push(("port_version", json_str(requirement)));
            if !url.is_empty() {
                f.push(("url", json_str(url)));
            }
        }
        Reason::Plugin { plugin } => f.push(("plugin", json_str(plugin))),
    }
    if !req.server_name.is_empty() {
        f.push(("server_name", json_str(&req.server_name)));
    }
    f.push(("client_version", json_str(shared::protocol::port::VERSION)));
    let body: Vec<String> = f.into_iter().map(|(k, v)| format!("{}:{v}", json_str(k))).collect();
    format!("{{{}}}\n", body.join(","))
}

/// True for an answer whose `"ok"` member is `true`. Anything else (`false`, garbage, nothing)
/// means the launcher didn't take the request.
pub fn answer_ok(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('{') {
        return false;
    }
    let Some(i) = t.find("\"ok\"") else { return false };
    let rest = t[i + 4..].trim_start();
    let Some(rest) = rest.strip_prefix(':') else { return false };
    let rest = rest.trim_start();
    rest.starts_with("true") && rest[4..].trim_start().starts_with([',', '}'])
}

/// The endpoint must look like this platform's: an absolute socket path, or a pipe name.
pub fn check_endpoint(ep: &str) -> Result<(), String> {
    let ok = if cfg!(windows) { ep.starts_with(r"\\.\pipe\") && ep.len() > 9 } else { ep.starts_with('/') };
    if ok && !ep.contains('\0') { Ok(()) } else { Err(format!("TSMOD_LAUNCHER_IPC isn't an IPC endpoint here: {ep:?}")) }
}

/// Sends `line` and waits up to [`TIMEOUT`] for an `ok` answer. The exchange runs on a thread, so
/// a launcher that accepts and never answers costs only the timeout.
pub fn send(endpoint: &str, line: &str) -> Result<(), String> {
    check_endpoint(endpoint)?;
    let (ep, line) = (endpoint.to_owned(), line.to_owned());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(exchange(&ep, &line));
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(answer)) if answer_ok(&answer) => Ok(()),
        Ok(Ok(answer)) => Err(format!("the launcher declined: {}", answer.trim())),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("no answer within {} s", TIMEOUT.as_secs())),
    }
}

#[cfg(unix)]
fn exchange(ep: &str, line: &str) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::MetadataExt;
    // Only a socket of this user's: another user could otherwise collect the request.
    let me = std::fs::metadata("/proc/self").map(|m| m.uid()).ok();
    let owner = std::fs::symlink_metadata(ep).map_err(|e| format!("{ep}: {e}"))?.uid();
    if me.is_some_and(|me| me != owner) {
        return Err(format!("{ep} belongs to another user"));
    }
    let mut s = std::os::unix::net::UnixStream::connect(ep).map_err(|e| format!("{ep}: {e}"))?;
    let _ = s.set_read_timeout(Some(TIMEOUT));
    let _ = s.set_write_timeout(Some(TIMEOUT));
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    BufReader::new(s.take(4096)).read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(answer)
}

#[cfg(windows)]
fn exchange(ep: &str, line: &str) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Read, Write};
    // The launcher creates the pipe for the current user only (§13.5); std opens it like a file.
    let mut p = std::fs::OpenOptions::new().read(true).write(true).open(ep).map_err(|e| format!("{ep}: {e}"))?;
    p.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut answer = String::new();
    BufReader::new(p.take(4096)).read_line(&mut answer).map_err(|e| e.to_string())?;
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines() {
        let r = Request::new(
            "203.0.113.5:27015",
            "Bob's \"TS\"",
            true,
            Reason::Port { requirement: ">=3.0.8, <3.1".into(), url: "https://tsmod.org".into() },
        )
        .unwrap();
        let line = request_line(&r, Some("ab12"), 41733);
        assert!(line.ends_with("}\n") && !line[..line.len() - 1].contains('\n'), "{line}");
        assert!(line.starts_with(r#"{"op":"relaunch","v":1,"instance":"ab12","pid":41733,"connect":"203.0.113.5:27015","password_required":true,"reason":"port","port_version":">=3.0.8, <3.1","url":"https://tsmod.org","server_name":"Bob's \"TS\"","client_version":""#), "{line}");
        let r = Request::new("203.0.113.5:27015", "", false, Reason::Restart).unwrap();
        let line = request_line(&r, None, 1);
        assert!(!line.contains("instance") && !line.contains("server_name") && line.contains(r#""password_required":false"#), "{line}");
    }

    #[test]
    fn answers() {
        assert!(answer_ok("{\"ok\":true}\n"));
        assert!(answer_ok(" { \"ok\" : true , \"x\":1}"));
        for no in ["", "{\"ok\":false,\"error\":\"busy\"}", "ok", "{\"ok\":trueish}", "{\"okay\":true}", "\"ok\":true"] {
            assert!(!answer_ok(no), "{no:?}");
        }
    }

    #[test]
    fn endpoints() {
        if cfg!(windows) {
            assert!(check_endpoint(r"\\.\pipe\ts-launcher-S-1-5-21").is_ok());
            assert!(check_endpoint("C:\\x").is_err());
        } else {
            assert!(check_endpoint("/run/user/1000/ts-launcher-1000.sock").is_ok());
            assert!(check_endpoint("relative.sock").is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_socket_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("tsrs-ipc-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("sock");
        let _ = std::fs::remove_file(&path);
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let t = std::thread::spawn(move || {
            let (s, _) = l.accept().unwrap();
            let mut got = String::new();
            BufReader::new(s.try_clone().unwrap()).read_line(&mut got).unwrap();
            (&s).write_all(b"{\"ok\":true}\n").unwrap();
            got
        });
        let r = Request::new("203.0.113.5:27015", "", true, Reason::Restart).unwrap();
        let line = request_line(&r, Some("e2e"), 5);
        assert_eq!(send(path.to_str().unwrap(), &line), Ok(()));
        assert_eq!(t.join().unwrap(), line);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
