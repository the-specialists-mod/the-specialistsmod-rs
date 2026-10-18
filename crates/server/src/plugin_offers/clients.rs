//! What the server keeps per client for offers (§6.5, §13): the offers it sent, the client's
//! `tspkg` answers and plugin list, the rate limit, and a pending op 5 notice. Engine-free, so
//! the rules are unit-tested; `super` does the sending and kicking.

use shared::protocol::tspkg::{PORT_ID, Reply, State};
use std::collections::{BTreeMap, VecDeque};

/// `tspkg` commands a client may send per [`RATE_WINDOW`] seconds; the rest are ignored.
pub const RATE_LIMIT: usize = 16;
pub const RATE_WINDOW: f32 = 10.0;

/// A sliding-window limit on a client's `tspkg` commands.
#[derive(Clone, Debug, Default)]
pub struct RateLimit {
    times: VecDeque<f32>,
}

impl RateLimit {
    /// Counts a command at `now`; false when it's over the limit. A clock that went back (a new
    /// map restarts the server's time) clears the window.
    pub fn allow(&mut self, now: f32) -> bool {
        if self.times.back().is_some_and(|&t| t > now) {
            self.times.clear();
        }
        while self.times.front().is_some_and(|&t| now - t >= RATE_WINDOW) {
            self.times.pop_front();
        }
        if self.times.len() >= RATE_LIMIT {
            return false;
        }
        self.times.push_back(now);
        true
    }
}

/// The op 5 `port_version` notice for a port client whose version the server doesn't accept
/// (§13.1): sent when it's put in the server, then a kick with `reason` at `kick_at` unless it
/// has left.
#[derive(Clone, Debug, PartialEq)]
pub struct PortNotice {
    /// The requirement in semver syntax (the refusal's tag).
    pub requirement: String,
    pub url: String,
    /// The refusal text, the kick's reason.
    pub reason: String,
    /// Server time of the kick, once the notice is sent.
    pub kick_at: Option<f32>,
}

/// An offer as sent to this client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub version: String,
    pub required: bool,
}

/// One client's offer state.
#[derive(Clone, Debug, Default)]
pub struct Client {
    /// Offers sent this connection, by id.
    pub sent: BTreeMap<String, Sent>,
    /// The last answer per id: version, state, reason.
    pub answers: BTreeMap<String, (String, State, String)>,
    /// The loaded plugins `tspkg list` named, and whether its closing line came.
    pub plugins: Vec<(String, String)>,
    pub plugins_complete: bool,
    pub notice: Option<PortNotice>,
    /// Put in the server, and op 5 and the offers not sent yet: they go at the player's first
    /// `PlayerPostThink` (`plugin_offers::client_ready`).
    pub put_pending: bool,
    pub rate: RateLimit,
    rate_logged: bool,
    /// `match`, `[[block]]` and `unlisted` (`super::enforce`).
    pub enforce: super::enforce::Enforcement,
}

/// What a `tspkg` command did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Ignored, with the reason for the log; `None` for a reason already logged (the rate limit).
    Ignored(Option<String>),
    /// An answer to an offer; `kick` when it refuses a required offer and the server kicks for
    /// that (`sv_plugin_required_kick`).
    Answer { id: String, version: String, state: State, reason: String, kick: bool },
    /// A `tspkg list` line; `complete` on the closing line that completed the list (the first one
    /// this connection: the plugin API's `client_plugin_list` fires then).
    List { complete: bool },
}

impl Client {
    /// Handles a `tspkg` command's arguments (after `tspkg`). `hooks`: the client hooks `TSPkg`
    /// (`plugin_offers::hooks_tspkg`), the only clients whose plugin list counts.
    pub fn command(&mut self, now: f32, args: &[&str], required_kick: bool, hooks: bool) -> Outcome {
        if !self.rate.allow(now) {
            if std::mem::replace(&mut self.rate_logged, true) {
                return Outcome::Ignored(None);
            }
            return Outcome::Ignored(Some(format!("over {RATE_LIMIT} tspkg commands in {RATE_WINDOW} s; ignoring the rest")));
        }
        self.rate_logged = false;
        let reply = match Reply::parse(args) {
            Ok(r) => r,
            Err(e) => return Outcome::Ignored(Some(format!("bad tspkg command: {e}"))),
        };
        match reply {
            Reply::List(items) => {
                // A client that hooks `TSPkg` may list its plugins before any `TSPkg` (it does when
                // the server's version says it understands `tspkg`, §13.2), so a server with
                // only blocks or a policy, and nothing to offer, still learns them.
                if !hooks && self.sent.is_empty() && self.notice.is_none() {
                    return Outcome::Ignored(Some("tspkg list from a client that doesn't hook TSPkg".into()));
                }
                let complete = items.is_empty() && !self.plugins_complete;
                for item in items {
                    if !self.plugins.contains(&item) {
                        self.plugins.push(item);
                    }
                }
                self.plugins_complete |= complete;
                Outcome::List { complete }
            }
            Reply::State { id, version, state, reason } => {
                let required = match self.sent.get(&id) {
                    Some(s) => s.required,
                    None if id == PORT_ID && self.notice.is_some() => false,
                    None if self.enforce.targets.contains_key(&id) => false,
                    None => return Outcome::Ignored(Some(format!("tspkg for {id}, which the server didn't offer"))),
                };
                if state == State::Suspended && self.enforce.awaiting.get(&id).is_some_and(|(v, _)| *v == version) {
                    self.enforce.awaiting.remove(&id);
                }
                self.answers.insert(id.clone(), (version.clone(), state, reason.clone()));
                let kick = required && required_kick && state.refuses();
                Outcome::Answer { id, version, state, reason, kick }
            }
        }
    }
}

impl Client {
    /// The version of `id` this client has loaded, as far as the server knows (the plugin API's
    /// `client_plugin_version`, client-plugins.md §13.3): an answer `loaded` (the version it
    /// loaded since the list), else the list's, else an answer `have` (the same manifest approved
    /// already, which the client loads at once unless another version is loaded, and that one is
    /// in the list).
    pub fn loaded_version(&self, id: &str) -> Option<&str> {
        let answer = self.answers.get(id);
        if let Some((v, State::Loaded, _)) = answer {
            return Some(v);
        }
        if let Some((_, v)) = self.plugins.iter().find(|(i, _)| i == id) {
            return Some(v);
        }
        answer.filter(|(_, s, _)| *s == State::Have).map(|(v, _, _)| v.as_str())
    }
}

/// The kick reason for a refused required offer (§6.1): the plugin and its homepage.
pub fn required_kick_reason(name: &str, version: &str, homepage: Option<&str>) -> String {
    match homepage {
        Some(h) => format!("This server requires the client plugin {name} {version} ({h})."),
        None => format!("This server requires the client plugin {name} {version}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(required: bool) -> Client {
        let mut c = Client::default();
        c.sent.insert("webview".into(), Sent { version: "0.1.0".into(), required });
        c
    }

    fn args(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }

    #[test]
    fn answers_to_offers() {
        let mut c = offered(false);
        assert_eq!(
            c.command(1.0, &args("webview 0.1.0 offered"), true, false),
            Outcome::Answer { id: "webview".into(), version: "0.1.0".into(), state: State::Offered, reason: "".into(), kick: false }
        );
        assert_eq!(c.answers["webview"].1, State::Offered);
        let Outcome::Answer { kick, .. } = c.command(2.0, &args("webview 0.1.0 denied"), true, false) else { panic!() };
        assert!(!kick, "not required");
        assert!(matches!(c.command(3.0, &args("other 1.0.0 loaded"), true, false), Outcome::Ignored(Some(_))), "not offered");
        assert!(matches!(c.command(3.0, &args("webview 0.1.0 approved"), true, false), Outcome::Ignored(Some(_))), "bad state");
        assert!(matches!(c.command(3.0, &args("tsrs 3.0.8 restart"), true, true), Outcome::Ignored(Some(_))), "no op 5 sent");
        c.notice = Some(PortNotice { requirement: "=3.0.9".into(), url: "x".into(), reason: "r".into(), kick_at: None });
        assert!(matches!(c.command(3.0, &args("tsrs 3.0.8 restart"), true, true), Outcome::Answer { kick: false, .. }));
    }

    #[test]
    fn required_offers_kick_on_refusal() {
        for st in State::ALL {
            let mut c = offered(true);
            let Outcome::Answer { kick, .. } = c.command(1.0, &args(&format!("webview 0.1.0 {st} why")), true, false) else { panic!() };
            assert_eq!(kick, matches!(st, State::Denied | State::Unsupported | State::Failed), "{st}");
            let Outcome::Answer { kick, .. } = offered(true).command(1.0, &args(&format!("webview 0.1.0 {st}")), false, false) else {
                panic!()
            };
            assert!(!kick, "sv_plugin_required_kick 0");
        }
        assert_eq!(
            required_kick_reason("Web overlay", "0.1.0", Some("https://x.org")),
            "This server requires the client plugin Web overlay 0.1.0 (https://x.org)."
        );
    }

    #[test]
    fn answers_to_blocks_and_requires() {
        let mut c = offered(false);
        assert!(matches!(c.command(1.0, &args("manual:dev 0.0.0 suspended"), false, true), Outcome::Ignored(Some(_))), "no op 4 sent");
        c.enforce.targets.insert("manual:dev".into(), "0.0.0".into());
        c.enforce.awaiting.insert("manual:dev".into(), ("0.0.0".into(), 30.0));
        assert!(matches!(c.command(1.0, &args("manual:dev 0.1.0 suspended"), false, true), Outcome::Answer { kick: false, .. }));
        assert!(c.enforce.awaiting.contains_key("manual:dev"), "another version");
        assert!(matches!(c.command(1.0, &args("manual:dev 0.0.0 suspended x"), true, true), Outcome::Answer { kick: false, .. }));
        assert!(c.enforce.awaiting.is_empty());
    }

    #[test]
    fn plugin_lists() {
        let mut c = Client::default();
        assert!(matches!(c.command(1.0, &args("list a@1.0.0"), false, false), Outcome::Ignored(Some(_))), "before any TSPkg");
        assert_eq!(c.command(1.0, &args("list a@1.0.0"), false, true), Outcome::List { complete: false }, "from a client that hooks it");
        let mut c = offered(false);
        assert_eq!(c.command(1.0, &args("list a@1.0.0 manual:x@0.0.0"), false, false), Outcome::List { complete: false });
        assert_eq!(c.command(1.0, &args("list a@1.0.0 b@2.0.0"), false, false), Outcome::List { complete: false });
        assert!(!c.plugins_complete);
        assert_eq!(c.command(1.0, &args("list"), false, false), Outcome::List { complete: true });
        assert!(c.plugins_complete);
        assert_eq!(c.plugins.len(), 3);
        assert_eq!(c.command(1.0, &args("list"), false, false), Outcome::List { complete: false }, "completed once");
        assert_eq!(c.command(1.0, &args("list c@1.0.0"), false, false), Outcome::List { complete: false });
    }

    #[test]
    fn loaded_versions() {
        let mut c = offered(false);
        assert_eq!(c.loaded_version("webview"), None);
        c.command(1.0, &args("webview 0.1.0 offered"), false, true);
        assert_eq!(c.loaded_version("webview"), None, "offered isn't loaded");
        c.command(1.0, &args("webview 0.1.0 have"), false, true);
        assert_eq!(c.loaded_version("webview"), Some("0.1.0"), "approved here: loaded at once");
        c.command(1.0, &args("list webview@0.0.9 manual:dev@0.0.0"), false, true);
        assert_eq!(c.loaded_version("webview"), Some("0.0.9"), "the list knows better than have");
        assert_eq!(c.loaded_version("manual:dev"), Some("0.0.0"));
        c.command(1.0, &args("webview 0.1.0 restart"), false, true);
        assert_eq!(c.loaded_version("webview"), Some("0.0.9"), "still the old one until the restart");
        c.command(1.0, &args("webview 0.1.0 loaded"), false, true);
        assert_eq!(c.loaded_version("webview"), Some("0.1.0"), "loaded since the list");
        assert_eq!(c.loaded_version("other"), None);
    }

    #[test]
    fn rate_limit() {
        let mut c = offered(false);
        for i in 0..RATE_LIMIT {
            assert!(matches!(c.command(i as f32 * 0.1, &args("webview 0.1.0 offered"), false, false), Outcome::Answer { .. }));
        }
        assert!(matches!(c.command(2.0, &args("webview 0.1.0 offered"), false, false), Outcome::Ignored(Some(_))), "logged once");
        assert_eq!(c.command(2.1, &args("webview 0.1.0 offered"), false, false), Outcome::Ignored(None));
        // The window slides: the first command is 10 s old at 10.0.
        assert!(matches!(c.command(10.0, &args("webview 0.1.0 offered"), false, false), Outcome::Answer { .. }));
        assert_eq!(
            c.command(10.0, &args("webview 0.1.0 offered"), false, false),
            Outcome::Ignored(Some(format!("over {RATE_LIMIT} tspkg commands in {RATE_WINDOW} s; ignoring the rest")))
        );
        // A new map's clock starts again.
        assert!(matches!(c.command(1.0, &args("webview 0.1.0 offered"), false, false), Outcome::Answer { .. }));
        let mut r = RateLimit::default();
        assert!((0..RATE_LIMIT).all(|_| r.allow(5.0)) && !r.allow(5.0) && r.allow(15.0));
    }
}
