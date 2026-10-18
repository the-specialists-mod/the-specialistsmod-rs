//! Port addition: version matching for the port's client at `ClientConnect`
//! (`docs/implementation/client-plugins.md` §13.1, issue #300). TS 3.0 checks no client version.
//!
//! `sv_client_version` says which port clients may join ([`Policy`]); `sv_client_require_port`
//! also refuses clients without the port's `_tsrs` userinfo key (TS 3.0's own client). A refusal
//! names both versions and ends with a machine-readable `[tsrs:<requirement>]` tag
//! ([`refusal`]). This is for compatibility, not anti-cheat: a client can send any version.

use crate::gameplay::server::ConnectingClient;
use shared::protocol::port;
use std::fmt;

/// `sv_client_version` (port addition): which port client versions may join.
pub const CVAR_VERSION: &std::ffi::CStr = c"sv_client_version";
/// `sv_client_require_port` (port addition): 1 also refuses clients without `_tsrs`.
pub const CVAR_REQUIRE_PORT: &std::ffi::CStr = c"sv_client_require_port";
/// `sv_client_version_url` (port addition): where the refusal says to get the port; empty for
/// [`DEFAULT_URL`].
pub const CVAR_URL: &std::ffi::CStr = c"sv_client_version_url";
/// The site a refusal names when `sv_client_version_url` is empty (the 2.1 mode's refusal names
/// it too).
pub const DEFAULT_URL: &str = "tsmod.org";

/// The bytes a reason may have: the engines' `szRejectReason` is 128 bytes with the NUL (FWGS
/// `SV_New_f`, ReHLDS `SV_ConnectClient_internal`, which reimplements HLDS's).
pub const REASON_MAX: usize = 127;

/// A `sv_client_version` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Policy {
    /// `any` (the default, and an empty value): every port client joins.
    Any,
    /// `same_minor`: the server's `major.minor` (the TS version, 3.0).
    SameMinor,
    /// `exact`: the server's own version.
    Exact,
    /// A semver requirement such as `>=3.0.8, <3.1`. `*` admits every version the server can
    /// read, but not a `_tsrs` value that isn't one.
    Req(semver::VersionReq),
}

impl Policy {
    /// Parses a cvar value (case-insensitive keywords). `Err` keeps the value for the log.
    pub fn parse(value: &str) -> Result<Policy, String> {
        let v = value.trim();
        match v.to_ascii_lowercase().as_str() {
            "" | "any" => Ok(Policy::Any),
            "same_minor" => Ok(Policy::SameMinor),
            "exact" => Ok(Policy::Exact),
            _ => semver::VersionReq::parse(v).map(Policy::Req).map_err(|_| v.to_owned()),
        }
    }

    /// Whether a client at `client` may join a server at `server`.
    pub fn accepts(&self, server: [i16; 3], client: [i16; 3]) -> bool {
        match self {
            Policy::Any => true,
            Policy::SameMinor => client[..2] == server[..2],
            Policy::Exact => client == server,
            Policy::Req(r) => r.matches(&semver_of(client)),
        }
    }

    /// The requirement for the refusal tag, in semver syntax: `*`, `~3.0`, `=3.0.8`, or the
    /// requirement as semver writes it.
    pub fn tag(&self, server: [i16; 3]) -> String {
        let [major, minor, patch] = server;
        match self {
            Policy::Any => "*".to_owned(),
            Policy::SameMinor => format!("~{major}.{minor}"),
            Policy::Exact => format!("={major}.{minor}.{patch}"),
            Policy::Req(r) => r.to_string(),
        }
    }

    /// What the refusal says the server needs: empty, `3.0.x`, `3.0.8`, or the requirement.
    fn needs(&self, server: [i16; 3]) -> String {
        let [major, minor, patch] = server;
        match self {
            Policy::Any => String::new(),
            Policy::SameMinor => format!(" {major}.{minor}.x"),
            Policy::Exact => format!(" {major}.{minor}.{patch}"),
            Policy::Req(r) => format!(" {r}"),
        }
    }
}

fn semver_of(v: [i16; 3]) -> semver::Version {
    let [major, minor, patch] = v.map(|n| n.max(0) as u64);
    semver::Version::new(major, minor, patch)
}

/// What the connecting client has, for the refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Have {
    /// No `_tsrs`: TS 3.0's own client, or a port client from before the key.
    Original,
    /// A `_tsrs` value that isn't a version (`port::parse_userinfo_version`).
    Unknown,
    Version([i16; 3]),
}

impl fmt::Display for Have {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Have::Original => f.write_str("TS 3.0"),
            Have::Unknown => f.write_str("unknown"),
            Have::Version([a, b, c]) => write!(f, "{a}.{b}.{c}"),
        }
    }
}

/// The server's settings for one check.
#[derive(Clone, Debug)]
pub struct Settings {
    pub policy: Policy,
    pub require_port: bool,
    /// `sv_client_version_url`; empty for [`DEFAULT_URL`].
    pub url: String,
}

/// What `ClientConnect` does with a client after the version check ([`decide`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Admit,
    /// Refused with this reason.
    Refuse(String),
    /// A port client that hooks `TSPkg` (§13.1): let in, sent op 5 with `requirement` (the tag's
    /// requirement), and kicked with `reason` after `sv_plugin_check_time` unless it leaves.
    Notify {
        requirement: String,
        reason: String,
    },
}

/// [`check`], except that a mismatching port client for which `hooks_tspkg` holds is let in and
/// notified instead (`plugin_offers`). Clients whose version can't be read are refused as before:
/// nothing says they hook `TSPkg`.
pub fn decide(s: &Settings, server: [i16; 3], client: ConnectingClient, hooks_tspkg: impl Fn([i16; 3]) -> bool) -> Verdict {
    let Some(reason) = check(s, server, client) else { return Verdict::Admit };
    let requirement = s.policy.tag(server);
    match client {
        ConnectingClient::Port(v)
            if port::parse_userinfo_version(v).is_some_and(hooks_tspkg)
                && requirement.len() <= shared::protocol::tspkg::MAX_REQUIREMENT =>
        {
            Verdict::Notify { requirement, reason }
        }
        _ => Verdict::Refuse(reason),
    }
}

/// The URL op 5 carries: `sv_client_version_url`, or [`DEFAULT_URL`].
pub fn notice_url(url: &str) -> String {
    let url = clean(url.trim());
    let url = url.trim();
    if url.is_empty() || url.len() > shared::protocol::tspkg::MAX_URL { DEFAULT_URL.to_owned() } else { url.to_owned() }
}

/// A reason for a `kick` command: cleaned like a refusal (no `;`, quotes or control characters)
/// and cut to [`REASON_MAX`] bytes at a character boundary.
pub fn clean_reason(reason: &str) -> String {
    cut(&clean(reason), REASON_MAX).trim_end().to_owned()
}

/// `ClientConnect`'s version check: `None` lets `client` in, `Some(reason)` refuses it with a
/// reason that fits the engines' reject buffer. Bots and HLTV are never checked.
pub fn check(s: &Settings, server: [i16; 3], client: ConnectingClient) -> Option<String> {
    let have = match client {
        ConnectingClient::Bot | ConnectingClient::Hltv => return None,
        ConnectingClient::Other if s.require_port => Have::Original,
        ConnectingClient::Other => return None,
        ConnectingClient::Port(_) if s.policy == Policy::Any => return None,
        ConnectingClient::Port(v) => match port::parse_userinfo_version(v) {
            Some(c) if s.policy.accepts(server, c) => return None,
            Some(c) => Have::Version(c),
            None => Have::Unknown,
        },
    };
    Some(refusal(&s.policy, server, have, &s.url))
}

/// The refusal text: "This server needs The Specialists port 3.0.8 (yours: 3.0.7). Update via
/// the TS launcher or tsmod.org. [tsrs:=3.0.8]". Cut to [`REASON_MAX`] bytes, keeping the tag
/// whole (with the default URL every policy but a long requirement fits), and cleaned for the engines: ReHLDS and HLDS also send it to the client as
/// `echo <reason>` console text, where a `;`, quote or line break would start another command.
pub fn refusal(policy: &Policy, server: [i16; 3], have: Have, url: &str) -> String {
    let url = if url.trim().is_empty() { DEFAULT_URL } else { url.trim() };
    let verb = if have == Have::Original { "Get it" } else { "Update" };
    let body =
        format!("This server needs The Specialists port{} (yours: {have}). {verb} via the TS launcher or {url}.", policy.needs(server));
    fit(&clean(&body), &clean(&format!("[tsrs:{}]", policy.tag(server))))
}

/// `body`, cut at a character boundary, then a space and `tag`, in [`REASON_MAX`] bytes. A tag
/// longer than that (a requirement over 100 bytes or so) is cut too.
fn fit(body: &str, tag: &str) -> String {
    let tag = cut(tag, REASON_MAX);
    let room = REASON_MAX.saturating_sub(tag.len() + 1);
    let body = cut(body, room).trim_end();
    if body.is_empty() { tag.to_owned() } else { format!("{body} {tag}") }
}

fn cut(s: &str, max: usize) -> &str {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// Replaces what the client's console would run as another command (`;`, `"`, line breaks and
/// other control characters) with spaces.
fn clean(s: &str) -> String {
    s.chars().map(|c| if c == ';' || c == '"' || c.is_control() { ' ' } else { c }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConnectingClient::*;

    const SERVER: [i16; 3] = [3, 0, 8];

    fn settings(policy: &str, require_port: bool) -> Settings {
        Settings { policy: Policy::parse(policy).unwrap(), require_port, url: String::new() }
    }

    #[test]
    fn parses_the_cvar() {
        assert_eq!(Policy::parse(""), Ok(Policy::Any));
        assert_eq!(Policy::parse(" ANY "), Ok(Policy::Any));
        assert_eq!(Policy::parse("same_minor"), Ok(Policy::SameMinor));
        assert_eq!(Policy::parse("Exact"), Ok(Policy::Exact));
        assert!(matches!(Policy::parse(">=3.0.8, <3.1"), Ok(Policy::Req(_))));
        assert!(matches!(Policy::parse("*"), Ok(Policy::Req(_))));
        assert_eq!(Policy::parse("newest"), Err("newest".to_owned()));
        assert_eq!(Policy::parse(">=3.0.8 <3.1 junk"), Err(">=3.0.8 <3.1 junk".to_owned()));
    }

    #[test]
    fn matching() {
        let p = |s: &str| Policy::parse(s).unwrap();
        assert!(p("any").accepts(SERVER, [9, 9, 9]));
        assert!(p("same_minor").accepts(SERVER, [3, 0, 0]));
        assert!(p("same_minor").accepts(SERVER, [3, 0, 99]));
        assert!(!p("same_minor").accepts(SERVER, [3, 1, 8]));
        assert!(!p("same_minor").accepts(SERVER, [2, 0, 8]));
        assert!(p("exact").accepts(SERVER, [3, 0, 8]));
        assert!(!p("exact").accepts(SERVER, [3, 0, 7]));
        assert!(!p("exact").accepts(SERVER, [3, 0, 9]));
        let r = p(">=3.0.8, <3.1");
        assert!(r.accepts(SERVER, [3, 0, 8]) && r.accepts(SERVER, [3, 0, 20]));
        assert!(!r.accepts(SERVER, [3, 0, 7]) && !r.accepts(SERVER, [3, 1, 0]));
        // Cargo's rules: a bare version is a caret requirement.
        assert!(p("3.0.8").accepts(SERVER, [3, 9, 0]) && !p("3.0.8").accepts(SERVER, [4, 0, 0]));
        assert!(p("*").accepts(SERVER, [0, 0, 1]));
    }

    #[test]
    fn tags() {
        let t = |s: &str| Policy::parse(s).unwrap().tag(SERVER);
        assert_eq!(t("any"), "*");
        assert_eq!(t("same_minor"), "~3.0");
        assert_eq!(t("exact"), "=3.0.8");
        assert_eq!(t(">=3.0.8,<3.1"), ">=3.0.8, <3.1");
        // Each tag reads back as a requirement with the same meaning.
        for (s, ok, bad) in [("same_minor", [3, 0, 1], [3, 1, 0]), ("exact", [3, 0, 8], [3, 0, 7])] {
            let r = Policy::parse(&t(s)).unwrap();
            assert!(r.accepts(SERVER, ok) && !r.accepts(SERVER, bad), "{s}");
        }
    }

    #[test]
    fn defaults_let_everyone_in() {
        let s = settings("any", false);
        for c in [Bot, Hltv, Other, Port("3.0.8"), Port("1"), Port("junk"), Port("9.9.9")] {
            assert_eq!(check(&s, SERVER, c), None, "{c:?}");
        }
    }

    #[test]
    fn who_is_refused() {
        let exact = settings("exact", false);
        assert_eq!(check(&exact, SERVER, Port("3.0.8")), None);
        assert_eq!(check(&exact, SERVER, Port("3000008")), None, "the numeric form a +connect sends first");
        assert_eq!(check(&exact, SERVER, Port("3.0.8-rc1")), None, "pre-release text is dropped");
        assert!(check(&exact, SERVER, Port("3.0.7")).unwrap().contains("(yours: 3.0.7)"));
        assert!(check(&exact, SERVER, Port("1")).unwrap().contains("(yours: unknown)"));
        assert!(check(&exact, SERVER, Port("junk")).is_some());
        assert_eq!(check(&exact, SERVER, Other), None, "TS 3.0's client only with sv_client_require_port");
        for c in [Bot, Hltv] {
            assert_eq!(check(&settings("exact", true), SERVER, c), None, "{c:?} never checked");
        }
        let port_only = settings("any", true);
        assert_eq!(check(&port_only, SERVER, Port("1")), None);
        assert_eq!(check(&port_only, SERVER, Port("0.0.1")), None);
        assert_eq!(
            check(&port_only, SERVER, Other).as_deref(),
            Some("This server needs The Specialists port (yours: TS 3.0). Get it via the TS launcher or tsmod.org. [tsrs:*]")
        );
    }

    #[test]
    fn refusal_text() {
        let r = |policy: &str, have, url: &str| refusal(&Policy::parse(policy).unwrap(), SERVER, have, url);
        assert_eq!(
            r("exact", Have::Version([3, 0, 7]), ""),
            "This server needs The Specialists port 3.0.8 (yours: 3.0.7). Update via the TS launcher or tsmod.org. [tsrs:=3.0.8]"
        );
        assert_eq!(
            r("same_minor", Have::Version([3, 1, 0]), " example.org "),
            "This server needs The Specialists port 3.0.x (yours: 3.1.0). Update via the TS launcher or example.org. [tsrs:~3.0]"
        );
        // Too long: the body is cut, the tag kept whole.
        let long = r(">=3.0.8, <3.1", Have::Unknown, "https://github.com/the-specialists-mod/the-specialistsmod-rs/releases");
        assert!(long.len() <= REASON_MAX, "{} bytes", long.len());
        assert!(long.starts_with("This server needs The Specialists port >=3.0.8, <3.1 (yours: unknown)."), "{long}");
        assert!(long.ends_with(" [tsrs:>=3.0.8, <3.1]"), "{long}");
        // Nothing the client's console would run as a command.
        let evil = r("any", Have::Original, "x;quit\n\"bind\"");
        assert!(!evil.contains([';', '"', '\n']), "{evil}");
        assert!(evil.contains("launcher or x quit  bind ."), "{evil}");
        // A multi-byte URL is cut at a character boundary.
        let wide = r("exact", Have::Version([3, 0, 7]), &"é".repeat(80));
        assert!(wide.len() <= REASON_MAX && wide.ends_with(" [tsrs:=3.0.8]"), "{wide}");
    }

    #[test]
    fn tspkg_clients_are_notified() {
        let exact = settings("exact", false);
        let hooks = port::hooks_tspkg;
        assert_eq!(decide(&exact, SERVER, Port("3.0.8"), hooks), Verdict::Admit);
        // 3.0.7 doesn't hook TSPkg: refused at connect, as before.
        assert!(matches!(decide(&exact, SERVER, Port("3.0.7"), hooks), Verdict::Refuse(_)));
        let Verdict::Notify { requirement, reason } = decide(&exact, SERVER, Port("3.0.9"), hooks) else { panic!() };
        assert_eq!(requirement, "=3.0.8");
        assert!(reason.contains("(yours: 3.0.9)") && reason.ends_with("[tsrs:=3.0.8]"), "{reason}");
        assert!(matches!(decide(&exact, SERVER, Port("3009000"), hooks), Verdict::Notify { .. }), "the numeric form");
        // Unknown versions and TS 3.0's client are refused whatever hooks_tspkg says.
        assert!(matches!(decide(&exact, SERVER, Port("junk"), |_| true), Verdict::Refuse(_)));
        assert!(matches!(decide(&settings("any", true), SERVER, Other, |_| true), Verdict::Refuse(_)));
        assert_eq!(decide(&exact, SERVER, Bot, |_| true), Verdict::Admit);
        // A requirement too long for op 5 refuses at connect instead.
        let huge = (0..30).map(|i| format!(">=1.{i}.0")).collect::<Vec<_>>().join(", ");
        assert!(matches!(decide(&settings(&huge, false), SERVER, Port("0.9.0"), |_| true), Verdict::Refuse(_)));
        assert_eq!(notice_url(""), DEFAULT_URL);
        assert_eq!(notice_url(" https://x.org/get;quit "), "https://x.org/get quit");
        assert_eq!(clean_reason("a;b\"c\nd"), "a b c d");
        assert!(clean_reason(&"é".repeat(100)).len() <= REASON_MAX);
    }

    #[test]
    fn fits_every_case() {
        let huge = (0..30).map(|i| format!(">=1.{i}.0")).collect::<Vec<_>>().join(", ");
        for policy in ["any", "same_minor", "exact", ">=3.0.8, <3.1", "*", &huge] {
            let p = Policy::parse(policy).unwrap();
            for have in [Have::Original, Have::Unknown, Have::Version([3, 0, 7]), Have::Version([15, 999, 999])] {
                let text = refusal(&p, SERVER, have, &"u".repeat(200));
                assert!(text.len() <= REASON_MAX, "{policy} {have:?}: {} bytes", text.len());
                assert!(text.contains("[tsrs:"), "{text}");
            }
        }
        assert_eq!(fit("", "[tsrs:*]"), "[tsrs:*]");
        // With the default URL, the keyword policies aren't cut for versions up to 3.10.10.
        for policy in ["any", "same_minor", "exact"] {
            let p = Policy::parse(policy).unwrap();
            for have in [Have::Original, Have::Unknown, Have::Version([3, 10, 10])] {
                let text = refusal(&p, [3, 10, 10], have, "");
                assert!(text.contains("tsmod.org. [tsrs:"), "{policy} {have:?}: {text}");
            }
        }
    }
}
