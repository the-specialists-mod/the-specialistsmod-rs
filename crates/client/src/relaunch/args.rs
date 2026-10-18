//! Command lines for restart and reconnect (docs/implementation/client-plugins.md §13.5): the
//! `ts-launcher relaunch` arguments, the self-relaunch's engine arguments, the Steam URL, and
//! `tsrs_reconnect`'s own arguments. None of them can carry a password: [`super::plan::Request`]
//! has no field for one, only `password_required`.

use super::plan::{Reason, Request};

/// `host:port`: a host name or IPv4 address (`[A-Za-z0-9.-]`, not starting with `-`), or a
/// bracketed IPv6 address, then a port 1..=65535. At most 64 bytes. This is what every command
/// line and IPC request carries, so nothing else can be smuggled in as an address.
pub fn valid_address(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let Some((host, port)) = s.rsplit_once(':') else { return false };
    let port_ok =
        !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) && matches!(port.parse::<u32>(), Ok(1..=65535));
    let host_ok = if let Some(v6) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !v6.is_empty() && v6.bytes().all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
    } else {
        !host.is_empty() && !host.starts_with('-') && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    };
    port_ok && host_ok
}

/// The engine command that reconnects after a restart: `+connect <address>`, or
/// `+tsrs_reconnect <address> password`, which asks for the password in a card first.
pub fn connect_args(req: &Request) -> Vec<String> {
    if req.password_required {
        vec!["+tsrs_reconnect".into(), req.connect.clone(), "password".into()]
    } else {
        vec!["+connect".into(), req.connect.clone()]
    }
}

/// `ts-launcher relaunch --instance ID --wait-pid PID --connect ADDR [--password-required]
/// [--reason R] [--port-version REQ] [--url URL] [--plugin ID@VERSION] [--server-name NAME]`
/// (the first argument is the subcommand; the launcher's path is the program).
pub fn launcher_args(req: &Request, instance: &str, pid: u32) -> Vec<String> {
    let mut a: Vec<String> = vec!["relaunch".into(), "--instance".into(), instance.into(), "--wait-pid".into(), pid.to_string()];
    a.extend(["--connect".into(), req.connect.clone()]);
    if req.password_required {
        a.push("--password-required".into());
    }
    a.extend(["--reason".into(), req.reason.name().into()]);
    match &req.reason {
        Reason::Restart => {}
        Reason::Port { requirement, url } => {
            a.extend(["--port-version".into(), requirement.clone()]);
            if !url.is_empty() {
                a.extend(["--url".into(), url.clone()]);
            }
        }
        Reason::Plugin { plugin } => a.extend(["--plugin".into(), plugin.clone()]),
    }
    if !req.server_name.is_empty() {
        a.extend(["--server-name".into(), req.server_name.clone()]);
    }
    a
}

/// The self-relaunch's arguments (after the program): the original engine parameters (`-game ts`,
/// `-windowed`, ...) without any console command (`+connect`, `+exec`, `+password`, `+map`: each
/// `+name` and the words after it up to the next `+` or `-` word), then [`connect_args`]. The
/// previous start's commands were one-off actions; repeating an `+exec` could run the same
/// script, restart included, again.
pub fn self_args(original: &[String], req: &Request) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_command = false;
    for a in original {
        if a.starts_with('+') {
            in_command = true;
        } else if a.starts_with('-') {
            in_command = false;
        }
        if !in_command {
            out.push(a.clone());
        }
    }
    out.extend(connect_args(req));
    out
}

/// `steam://run/70//<args>/`: Steam starts Half-Life (app 70) with `-game <gamedir>` and
/// [`connect_args`], spaces written `%20`.
pub fn steam_url(gamedir: &str, req: &Request) -> String {
    let mut words = vec!["-game".to_owned(), gamedir.to_owned()];
    words.extend(connect_args(req));
    format!("steam://run/70//{}/", words.join("%20"))
}

/// `tsrs_reconnect <address> [password]`: the address, and whether to ask for the password. The
/// second word may only be the literal `password`; anything else (a password typed as an argument)
/// is refused, so a password never goes through a command line.
pub fn parse_reconnect(args: &[&str]) -> Result<(String, bool), &'static str> {
    match args {
        [addr] if valid_address(addr) => Ok(((*addr).to_owned(), false)),
        [addr, "password"] if valid_address(addr) => Ok(((*addr).to_owned(), true)),
        [_, _] | [_] => Err("usage: tsrs_reconnect <host:port> [password] (the word \"password\" asks for it in a card)"),
        _ => Err("usage: tsrs_reconnect <host:port> [password]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(password: bool, reason: Reason) -> Request {
        Request::new("203.0.113.5:27015", "Bob's TS", password, reason).unwrap()
    }

    #[test]
    fn addresses() {
        for ok in ["203.0.113.5:27015", "ts.example.org:27015", "localhost:1", "[::1]:27015", "a-b.c:65535"] {
            assert!(valid_address(ok), "{ok}");
        }
        for bad in [
            "",
            "203.0.113.5",
            "203.0.113.5:0",
            "203.0.113.5:65536",
            "-x:27015",
            "a b:27015",
            "a;quit:27015",
            "a\":27015",
            "a:27015;quit",
            "a:27015\n",
            "[]:1",
            ":27015",
            "loopback",
        ] {
            assert!(!valid_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn launcher_arguments() {
        let a =
            launcher_args(&req(true, Reason::Port { requirement: ">=3.0.8, <3.1".into(), url: "https://tsmod.org".into() }), "ab12", 41733);
        assert_eq!(
            a,
            [
                "relaunch",
                "--instance",
                "ab12",
                "--wait-pid",
                "41733",
                "--connect",
                "203.0.113.5:27015",
                "--password-required",
                "--reason",
                "port",
                "--port-version",
                ">=3.0.8, <3.1",
                "--url",
                "https://tsmod.org",
                "--server-name",
                "Bob's TS"
            ]
        );
        let a = launcher_args(&req(false, Reason::Plugin { plugin: "webview@0.2.0".into() }), "ab12", 7);
        assert!(!a.contains(&"--password-required".to_owned()));
        assert!(a.windows(2).any(|w| w == ["--plugin", "webview@0.2.0"]));
        assert!(a.windows(2).any(|w| w == ["--reason", "plugin"]));
    }

    #[test]
    fn self_relaunch_drops_commands() {
        let orig: Vec<String> = [
            "-game",
            "ts",
            "-windowed",
            "+password",
            "secret",
            "+exec",
            "x.cfg",
            "-width",
            "800",
            "+connect",
            "1.2.3.4:27015",
            "+map",
            "ts_dojo",
        ]
        .map(String::from)
        .to_vec();
        let a = self_args(&orig, &req(true, Reason::Restart));
        assert_eq!(a, ["-game", "ts", "-windowed", "-width", "800", "+tsrs_reconnect", "203.0.113.5:27015", "password"]);
        assert!(!a.iter().any(|w| w.contains("secret")));
        let a = self_args(&orig, &req(false, Reason::Restart));
        assert_eq!(&a[a.len() - 2..], ["+connect", "203.0.113.5:27015"]);
    }

    #[test]
    fn steam() {
        assert_eq!(steam_url("ts", &req(false, Reason::Restart)), "steam://run/70//-game%20ts%20+connect%20203.0.113.5:27015/");
        assert_eq!(
            steam_url("ts", &req(true, Reason::Restart)),
            "steam://run/70//-game%20ts%20+tsrs_reconnect%20203.0.113.5:27015%20password/"
        );
    }

    #[test]
    fn reconnect_command() {
        assert_eq!(parse_reconnect(&["1.2.3.4:27015"]), Ok(("1.2.3.4:27015".into(), false)));
        assert_eq!(parse_reconnect(&["1.2.3.4:27015", "password"]), Ok(("1.2.3.4:27015".into(), true)));
        // A password given as the argument is refused, never used.
        assert!(parse_reconnect(&["1.2.3.4:27015", "hunter2"]).is_err());
        assert!(parse_reconnect(&["1.2.3.4:27015;quit"]).is_err());
        assert!(parse_reconnect(&[]).is_err());
        assert!(parse_reconnect(&["a:1", "password", "x"]).is_err());
    }
}
