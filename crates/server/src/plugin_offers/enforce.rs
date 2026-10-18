//! Enforcement of `match`, `[[block]]` and `[policy] unlisted` (client-plugins.md §13.2, §13.3;
//! issue #302). Engine-free, so the decisions and their timing are unit-tested; `super` sends the
//! messages, kicks and logs.
//!
//! For each client that hooks `TSPkg`, from the moment its offers went out (`ready_at`):
//!
//! 1. **Grace.** Nothing is decided until `sv_plugin_check_time` has passed, or the client's
//!    `tspkg list` is complete and every offer sent has a final answer (anything but `offered`
//!    and `downloading`). A list still incomplete then counts as what came (none: no plugins).
//! 2. **Each loaded plugin** the list names is judged once ([`judge`]): a matching `[[block]]`
//!    (the first, in file order) wins; else an offer for its id accepts the version through its
//!    `match` or gets op 3 `require` (with the `required` flag when the offer is); else it is
//!    unlisted, and `unlisted` other than `allow` blocks it with that action. A block's
//!    `suspend` sends op 4 and waits `sv_plugin_check_time` for `tspkg <id> <version>
//!    suspended`, then kicks; its `kick` sends op 4 and kicks at once.
//! 3. **Required offers**, with `sv_plugin_required_kick 1`: at `sv_plugin_download_time` after
//!    the offers went out, a required offer whose plugin isn't loaded at a version its `match`
//!    accepts kicks (a refusal kicks at once, `clients::Client::command`). The time lets a player
//!    who is downloading, or restarting for an update, finish.
//!
//! The server can only suspend or kick: it never approves or loads anything for the player (§7.3).

use super::clients::{Client, required_kick_reason};
use super::config::{Action, MatchRule, Offers, Unlisted};
use plugin_pkg::Version;
use shared::protocol::tspkg::{BlockAction, FLAG_REQUIRED, Message, State};
use std::collections::BTreeMap;

/// The op 4 reason for an unlisted plugin.
pub const UNLISTED_REASON: &str = "not allowed on this server";

/// What a loaded plugin gets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A matching offer accepts it, or it is unlisted and `unlisted = "allow"`.
    Allowed,
    /// An offer for its id doesn't accept its version: op 3 with the offer's rule.
    Require { requirement: String, required: bool, rule: String },
    /// A `[[block]]`, or `unlisted` other than `allow`: op 4.
    Block { action: Action, requirement: String, reason: String, rule: String },
}

/// The version a client reports, as semver; anything else counts as `0.0.0` (a hand-installed
/// library's version), so a client can't dodge a `<x` block with a token that isn't a version.
pub fn version_of(s: &str) -> Version {
    Version::parse(s).unwrap_or(Version::new(0, 0, 0))
}

/// Judges one loaded plugin (`id` as the client reports it: a plugin id or `manual:<name>`).
pub fn judge(file: &Offers, id: &str, version: &str) -> Verdict {
    let v = version_of(version);
    if let Some(b) = file.blocks.iter().find(|b| b.id.matches(id) && b.versions.matches(&v)) {
        let shown = match &b.id {
            super::config::BlockId::Plugin(p) => p.clone(),
            super::config::BlockId::Manual(n) => format!("manual:{n}"),
            super::config::BlockId::AnyManual => "manual:*".into(),
        };
        return Verdict::Block {
            action: b.action,
            requirement: b.versions.to_string(),
            reason: b.reason.clone(),
            rule: format!("[[block]] {shown} {}", b.versions),
        };
    }
    if let Some(o) = file.offers.iter().find(|o| o.id == id) {
        if o.match_rule.accepts(&o.version, &v) {
            return Verdict::Allowed;
        }
        let requirement = o.match_rule.requirement(&o.version);
        let rule = format!("match {} of the offer {} {}", rule_name(&o.match_rule), o.id, o.version);
        return Verdict::Require { requirement, required: o.required, rule };
    }
    let action = match file.unlisted {
        Unlisted::Allow => return Verdict::Allowed,
        Unlisted::Suspend => Action::Suspend,
        Unlisted::Kick => Action::Kick,
    };
    Verdict::Block { action, requirement: "*".into(), reason: UNLISTED_REASON.into(), rule: "[policy] unlisted".into() }
}

fn rule_name(m: &MatchRule) -> String {
    match m {
        MatchRule::Exact => "exact".into(),
        MatchRule::Compatible => "compatible".into(),
        MatchRule::Any => "any".into(),
        MatchRule::Req(r) => format!("\"{r}\""),
    }
}

/// The kick reason for a blocked plugin, naming it and its version.
pub fn block_kick_reason(id: &str, version: &str, reason: &str) -> String {
    if reason.is_empty() {
        format!("This server blocks the client plugin {id} {version}.")
    } else {
        format!("This server blocks the client plugin {id} {version}: {reason}")
    }
}

/// The kick reason for a block the client didn't suspend in time.
pub fn unsuspended_kick_reason(id: &str, version: &str) -> String {
    format!("This server blocks the client plugin {id} {version}, and it wasn't suspended.")
}

/// The seconds the rules wait (`sv_plugin_check_time`, `sv_plugin_download_time`).
#[derive(Clone, Copy, Debug)]
pub struct Times {
    pub check: f32,
    pub download: f32,
}

/// A checked offer's name and homepage, for the required kick's reason.
#[derive(Clone, Debug, Default)]
pub struct Named {
    pub name: String,
    pub homepage: Option<String>,
}

/// What [`step`] wants done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Send(Message),
    /// Kick with this reason (at most one per client).
    Kick(String),
    Log(String),
}

/// One client's enforcement state.
#[derive(Clone, Debug, Default)]
pub struct Enforcement {
    /// Server time the offers went out (`plugin_offers::client_ready`); `None`: the client
    /// doesn't hook `TSPkg`, and nothing is enforced.
    pub ready_at: Option<f32>,
    /// The loaded plugins were judged.
    pub judged: bool,
    /// Blocks sent with `suspend`, by id: the version, and the time a `suspended` answer is due.
    pub awaiting: BTreeMap<String, (String, f32)>,
    /// Ids the server sent op 3 or op 4 for: their `tspkg` answers are accepted.
    pub targets: BTreeMap<String, String>,
    /// The required offers were checked at `sv_plugin_download_time`.
    pub required_done: bool,
    /// A kick was asked for: nothing more happens.
    pub kicked: bool,
}

/// Whether the grace period (step 1 above) is over at `now`.
pub fn grace_over(c: &Client, now: f32, t: Times) -> bool {
    let Some(r) = c.enforce.ready_at else { return false };
    let answered = c.sent.keys().all(|id| c.answers.get(id).is_some_and(|(_, s, _)| !matches!(s, State::Offered | State::Downloading)));
    now >= r + t.check || (c.plugins_complete && answered)
}

/// The version of `id` the client has loaded now, as far as the server knows: an answer
/// `loaded` (the offered version, loaded since the list), else the list's, else `have` (the
/// same manifest approved here, which the client loads at once unless another version is).
fn loaded_version(c: &Client, id: &str, offered: &str) -> Option<String> {
    let answer = c.answers.get(id).filter(|(v, _, _)| v == offered).map(|(_, s, _)| *s);
    if answer == Some(State::Loaded) {
        return Some(offered.to_owned());
    }
    if let Some((_, v)) = c.plugins.iter().find(|(i, _)| i == id) {
        return Some(v.clone());
    }
    (answer == Some(State::Have)).then(|| offered.to_owned())
}

/// Advances `c`'s enforcement to `now` (each frame): what to send, log and kick.
pub fn step(c: &mut Client, now: f32, file: &Offers, named: &BTreeMap<String, Named>, t: Times, required_kick: bool) -> Vec<Act> {
    let mut acts = Vec::new();
    let Some(ready_at) = c.enforce.ready_at else { return acts };
    if c.enforce.kicked {
        return acts;
    }
    if !c.enforce.judged {
        if !grace_over(c, now, t) {
            return acts;
        }
        c.enforce.judged = true;
        if !c.plugins_complete {
            acts.push(Act::Log(if c.plugins.is_empty() {
                "no tspkg list in time: judged as having no plugins".into()
            } else {
                "the tspkg list didn't end in time: judged on what came".into()
            }));
        }
        for (id, version) in c.plugins.clone() {
            match judge(file, &id, &version) {
                Verdict::Allowed => {}
                Verdict::Require { requirement, required, rule } => {
                    acts.push(Act::Log(format!("{id} {version}: {rule}: require {requirement}")));
                    c.enforce.targets.insert(id.clone(), version.clone());
                    let flags = if required { FLAG_REQUIRED } else { 0 };
                    acts.push(Act::Send(Message::Require { id, requirement, flags }));
                }
                Verdict::Block { action, requirement, reason, rule } => {
                    let name = if action == Action::Kick { "kick" } else { "suspend" };
                    acts.push(Act::Log(format!("{id} {version}: {rule}: {name}")));
                    c.enforce.targets.insert(id.clone(), version.clone());
                    let wire = if action == Action::Kick { BlockAction::Kick } else { BlockAction::Suspend };
                    let kick = block_kick_reason(&id, &version, &reason);
                    acts.push(Act::Send(Message::Block { id: id.clone(), requirement, action: wire, reason }));
                    if action == Action::Kick {
                        c.enforce.kicked = true;
                        acts.push(Act::Kick(kick));
                        return acts;
                    }
                    c.enforce.awaiting.insert(id, (version, now + t.check));
                }
            }
        }
    }
    if let Some((id, (version, _))) = c.enforce.awaiting.iter().find(|(_, (_, due))| now >= *due) {
        acts.push(Act::Log(format!("{id} {version}: no \"suspended\" within sv_plugin_check_time")));
        acts.push(Act::Kick(unsuspended_kick_reason(id, version)));
        c.enforce.kicked = true;
        return acts;
    }
    if required_kick && !c.enforce.required_done && now >= ready_at + t.download {
        c.enforce.required_done = true;
        for (id, sent) in c.sent.iter().filter(|(_, s)| s.required) {
            let Some(o) = file.offers.iter().find(|o| o.id == *id) else { continue };
            let loaded = loaded_version(c, id, &sent.version);
            if loaded.as_deref().is_some_and(|v| o.match_rule.accepts(&o.version, &version_of(v))) {
                continue;
            }
            let n = named.get(id).cloned().unwrap_or_else(|| Named { name: id.clone(), homepage: None });
            acts.push(Act::Log(format!(
                "{id}: required, {} after sv_plugin_download_time",
                match &loaded {
                    Some(v) => format!("{v} loaded, which match {} doesn't accept,", rule_name(&o.match_rule)),
                    None => "not loaded".into(),
                }
            )));
            acts.push(Act::Kick(required_kick_reason(&n.name, &o.version.to_string(), n.homepage.as_deref())));
            c.enforce.kicked = true;
            return acts;
        }
    }
    acts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_offers::clients::Sent;
    use crate::plugin_offers::config::parse;

    const SHA: &str = "c3d1000000000000000000000000000000000000000000000000000000000000";
    const T: Times = Times { check: 15.0, download: 300.0 };

    fn file(extra: &str) -> Offers {
        let text = format!(
            "[[offer]]\nid = \"webview\"\nversion = \"0.2.0\"\nurl = \"p/webview.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\nrequired = true\n\n\
             [[offer]]\nid = \"stats\"\nversion = \"1.2.0\"\nurl = \"p/stats.tspkg\"\nsha256 = \"{SHA}\"\nsize = 1\nmatch = \"exact\"\n\n{extra}"
        );
        let (o, e) = parse(&text);
        assert!(e.is_empty(), "{e:?}");
        o
    }

    fn client(plugins: &[(&str, &str)]) -> Client {
        let mut c = Client::default();
        c.sent.insert("webview".into(), Sent { version: "0.2.0".into(), required: true });
        c.sent.insert("stats".into(), Sent { version: "1.2.0".into(), required: false });
        c.plugins = plugins.iter().map(|(i, v)| (i.to_string(), v.to_string())).collect();
        c.plugins_complete = true;
        c.enforce.ready_at = Some(10.0);
        c
    }

    fn answer(c: &mut Client, id: &str, version: &str, s: State) {
        c.answers.insert(id.into(), (version.into(), s, String::new()));
    }

    fn sends(acts: &[Act]) -> Vec<&Message> {
        acts.iter().filter_map(|a| if let Act::Send(m) = a { Some(m) } else { None }).collect()
    }

    fn kick(acts: &[Act]) -> Option<&str> {
        acts.iter().find_map(|a| if let Act::Kick(r) = a { Some(r.as_str()) } else { None })
    }

    #[test]
    fn verdicts() {
        let f = file(
            "[[block]]\nid = \"webview\"\nversions = \"<0.2.0\"\naction = \"kick\"\nreason = \"0.1.x breaks the vote page\"\n\n\
             [[block]]\nid = \"manual:*\"\nversions = \"*\"\n\n[policy]\nunlisted = \"suspend\"\n",
        );
        assert_eq!(judge(&f, "webview", "0.2.3"), Verdict::Allowed, "compatible");
        assert!(matches!(
            judge(&f, "webview", "0.1.9"),
            Verdict::Block { action: Action::Kick, ref requirement, ref rule, .. } if requirement == "<0.2.0" && rule == "[[block]] webview <0.2.0"
        ));
        assert_eq!(
            judge(&f, "webview", "0.3.0"),
            Verdict::Require { requirement: "^0.2.0".into(), required: true, rule: "match compatible of the offer webview 0.2.0".into() }
        );
        assert_eq!(judge(&f, "stats", "1.2.0"), Verdict::Allowed);
        assert!(
            matches!(judge(&f, "stats", "1.2.1"), Verdict::Require { required: false, ref requirement, .. } if requirement == "=1.2.0")
        );
        assert!(
            matches!(judge(&f, "manual:dev", "0.0.0"), Verdict::Block { action: Action::Suspend, ref rule, .. } if rule == "[[block]] manual:* *")
        );
        assert!(matches!(
            judge(&f, "other", "1.0.0"),
            Verdict::Block { action: Action::Suspend, ref rule, ref requirement, .. } if rule == "[policy] unlisted" && requirement == "*"
        ));
        // A version that isn't one counts as 0.0.0: still under `<0.2.0`.
        assert!(matches!(judge(&f, "webview", "latest"), Verdict::Block { action: Action::Kick, .. }));
        // Only `unlisted` and explicit manual blocks reach hand-installed libraries.
        let open = file("[[block]]\nid = \"dev\"\nversions = \"*\"\n");
        assert_eq!(judge(&open, "manual:dev", "0.0.0"), Verdict::Allowed);
        assert!(matches!(judge(&open, "dev", "0.1.0"), Verdict::Block { .. }));
        assert_eq!(judge(&open, "other", "1.0.0"), Verdict::Allowed, "unlisted allow is the default");
        let kick = file("[policy]\nunlisted = \"kick\"\n");
        assert!(matches!(judge(&kick, "manual:x", "0.0.0"), Verdict::Block { action: Action::Kick, .. }));
    }

    #[test]
    fn nothing_before_the_grace_period_ends() {
        let f = file("[policy]\nunlisted = \"kick\"\n");
        let mut c = client(&[("other", "1.0.0")]);
        answer(&mut c, "webview", "0.2.0", State::Downloading);
        answer(&mut c, "stats", "1.2.0", State::Have);
        assert!(step(&mut c, 11.0, &f, &BTreeMap::new(), T, true).is_empty(), "a download is running");
        answer(&mut c, "webview", "0.2.0", State::Offered);
        assert!(step(&mut c, 24.9, &f, &BTreeMap::new(), T, true).is_empty(), "a card is open");
        let acts = step(&mut c, 25.0, &f, &BTreeMap::new(), T, true);
        assert_eq!(kick(&acts), Some("This server blocks the client plugin other 1.0.0: not allowed on this server"));
        assert!(step(&mut c, 26.0, &f, &BTreeMap::new(), T, true).is_empty(), "one kick");
        // Every offer answered and the list complete: at once.
        let mut c = client(&[("other", "1.0.0")]);
        answer(&mut c, "webview", "0.2.0", State::Loaded);
        answer(&mut c, "stats", "1.2.0", State::Denied);
        assert!(kick(&step(&mut c, 10.5, &f, &BTreeMap::new(), T, true)).is_some());
        // A client that doesn't hook TSPkg (no ready_at) is never judged.
        let mut c = client(&[("other", "1.0.0")]);
        c.enforce.ready_at = None;
        assert!(step(&mut c, 1000.0, &f, &BTreeMap::new(), T, true).is_empty());
    }

    #[test]
    fn suspend_waits_for_the_answer() {
        let f = file("[[block]]\nid = \"webview\"\nversions = \"*\"\nreason = \"no overlays\"\n");
        let mut c = client(&[("webview", "0.2.0")]);
        c.plugins_complete = false;
        let acts = step(&mut c, 25.0, &f, &BTreeMap::new(), T, false);
        assert_eq!(
            sends(&acts),
            [&Message::Block { id: "webview".into(), requirement: "*".into(), action: BlockAction::Suspend, reason: "no overlays".into() }]
        );
        assert!(acts.contains(&Act::Log("the tspkg list didn't end in time: judged on what came".into())));
        assert_eq!(kick(&acts), None);
        assert!(c.enforce.targets.contains_key("webview"));
        // The answer (clients::Client::command) clears it.
        let mut answered = c.clone();
        answered.enforce.awaiting.clear();
        assert!(step(&mut answered, 100.0, &f, &BTreeMap::new(), T, false).is_empty());
        assert!(step(&mut c, 39.9, &f, &BTreeMap::new(), T, false).is_empty());
        assert_eq!(kick(&step(&mut c, 40.0, &f, &BTreeMap::new(), T, false)), Some(unsuspended_kick_reason("webview", "0.2.0").as_str()));
    }

    #[test]
    fn block_kicks_at_once() {
        let f = file("[[block]]\nid = \"webview\"\nversions = \"<0.3\"\naction = \"kick\"\n");
        let mut c = client(&[("webview", "0.2.0")]);
        answer(&mut c, "webview", "0.2.0", State::Have);
        answer(&mut c, "stats", "1.2.0", State::Denied);
        let acts = step(&mut c, 10.5, &f, &BTreeMap::new(), T, false);
        assert!(matches!(sends(&acts)[..], [Message::Block { action: BlockAction::Kick, .. }]), "op 4 first, then the kick");
        assert_eq!(kick(&acts), Some("This server blocks the client plugin webview 0.2.0."));
        assert!(acts.contains(&Act::Log("webview 0.2.0: [[block]] webview <0.3: kick".into())));
    }

    #[test]
    fn required_mismatch_gets_op_3_then_the_grace_kick() {
        let f = file("");
        let named: BTreeMap<String, Named> =
            [("webview".to_string(), Named { name: "Web overlay".into(), homepage: Some("https://x.org".into()) })].into();
        let mut c = client(&[("webview", "0.1.0"), ("stats", "1.2.0")]);
        answer(&mut c, "stats", "1.2.0", State::Have);
        answer(&mut c, "webview", "0.2.0", State::Restart);
        let acts = step(&mut c, 11.0, &f, &named, T, true);
        assert_eq!(sends(&acts), [&Message::Require { id: "webview".into(), requirement: "^0.2.0".into(), flags: FLAG_REQUIRED }]);
        assert!(step(&mut c, 309.9, &f, &named, T, true).is_empty(), "restarting for the update");
        let acts = step(&mut c, 310.0, &f, &named, T, true);
        assert_eq!(kick(&acts), Some("This server requires the client plugin Web overlay 0.2.0 (https://x.org)."));
        // Without sv_plugin_required_kick, only op 3.
        let mut c = client(&[("webview", "0.1.0")]);
        assert!(kick(&step(&mut c, 400.0, &f, &named, T, false)).is_none());
        // Loaded since the list, or approved and loaded at once: satisfied.
        for s in [State::Loaded, State::Have] {
            let mut c = client(&[]);
            answer(&mut c, "webview", "0.2.0", s);
            assert!(kick(&step(&mut c, 400.0, &f, &named, T, true)).is_none(), "{s}");
        }
        // `have` while the list shows an older version loaded isn't.
        let mut c = client(&[("webview", "0.1.0")]);
        answer(&mut c, "webview", "0.2.0", State::Have);
        assert!(kick(&step(&mut c, 400.0, &f, &named, T, true)).is_some());
        // Never answered, never listed: kicked at the download time, not before.
        let mut c = client(&[]);
        assert!(kick(&step(&mut c, 309.0, &f, &named, T, true)).is_none());
        assert!(kick(&step(&mut c, 310.0, &f, &named, T, true)).is_some());
        // A non-required mismatch is only told.
        let mut c = client(&[("stats", "1.3.0")]);
        answer(&mut c, "webview", "0.2.0", State::Loaded);
        let acts = step(&mut c, 400.0, &f, &named, T, true);
        assert_eq!(sends(&acts), [&Message::Require { id: "stats".into(), requirement: "=1.2.0".into(), flags: 0 }]);
        assert!(kick(&acts).is_none());
    }

    #[test]
    fn reasons_fit_the_kick() {
        let r = block_kick_reason(&"x".repeat(40), "1.0.0-alpha.123", &"r".repeat(64));
        assert!(crate::rules::client_version::clean_reason(&r).len() <= 127);
    }
}
