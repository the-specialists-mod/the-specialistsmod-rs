//! Port addition: server offers of client plugins (`docs/implementation/client-plugins.md` §6.1,
//! §6.2, §6.5, §12, §13.1; issue #298). TS 3.0 has no client plugins.
//!
//! * At each map start the server reads `ts/plugin_offers.toml` ([`config`]) and checks every
//!   offered package on its disk ([`check`]); a package that fails isn't offered, and the log
//!   says why. The checks are cached on the package's path, size and modification time, so an
//!   unchanged package isn't hashed again at every map.
//! * When a client is put in the server, it gets every offer as `TSPkg` op 1, if it hooks
//!   `TSPkg` ([`hooks_tspkg`]); `sv_plugin_offer` and `sv_plugin_withdraw` send one on demand.
//! * A port client whose version `sv_client_version` doesn't accept, but which hooks `TSPkg`,
//!   is let in, gets op 5 `port_version`, and is kicked with the refusal text after
//!   `sv_plugin_check_time` unless it has left (§13.1, `rules::client_version`).
//! * The client's `tspkg` answers ([`clients`]) are rate-limited, logged, and kept per client;
//!   `sv_plugin_required_kick 1` kicks a player who refuses a required offer.
//!
//! * `match`, `[[block]]` and `[policy] unlisted` are enforced on the client's `tspkg list`
//!   ([`enforce`], §13.2, §13.3, issue #302): op 3 `require` for a version an offer's `match`
//!   doesn't accept, op 4 `block` (suspend, or kick) for a blocked or unlisted plugin, and the
//!   required-offer kick at `sv_plugin_download_time`. The server can suspend or kick; it can
//!   never approve or load anything for the player (§7.3).
//!
//! The plugin API (version 8, issue #321, `plugin_api::client_plugins`) offers through
//! [`offer`] (`offer_client_plugin`), reads [`loaded_version`] (`client_plugin_version`), and
//! hears every answer and each completed list from [`client_command`] (`client_plugin_state`,
//! `client_plugin_list`). An AMX Mod X plugin also sees the `tspkg` client command first.

pub mod check;
pub mod clients;
pub mod config;
pub mod enforce;

use crate::commands::Args;
use crate::rules::cvars;
use clients::{Client, Outcome, PortNotice, Sent};
use goldsrc::Global;
use goldsrc::edict::EdictPtr;
use goldsrc::ext::weapons::WeaponsEngine;
use goldsrc::server::engine;
use goldsrc::sys::*;
use shared::protocol::{port, tspkg};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// `sv_plugin_required_kick` (port addition): 1 kicks a player who answers `denied`,
/// `unsupported` or `failed` to a required offer.
pub const CVAR_REQUIRED_KICK: &std::ffi::CStr = c"sv_plugin_required_kick";
/// `sv_plugin_check_time` (port addition): seconds a client has to answer before the server acts
/// (here: the op 5 kick).
pub const CVAR_CHECK_TIME: &std::ffi::CStr = c"sv_plugin_check_time";
/// `sv_plugin_download_time` (port addition): seconds after the offers went out before a
/// required offer that isn't loaded at a version its `match` accepts kicks (§13.3).
pub const CVAR_DOWNLOAD_TIME: &std::ffi::CStr = c"sv_plugin_download_time";
/// `ts_dev_tspkg` (port dev aid, needs `sv_cheats 1`): every port client with a readable
/// version counts as hooking `TSPkg`, so builds between releases (still reporting the last
/// release's version, below `TSPKG_SINCE`) can be tested.
pub const CVAR_DEV: &std::ffi::CStr = c"ts_dev_tspkg";

/// An offer that passed its checks.
struct Ready {
    offer: config::Offer,
    checked: check::Checked,
}

/// A check's result, for the package's size and modification time and the offer it was made for.
struct Cached {
    size: u64,
    modified: Option<SystemTime>,
    offer: config::Offer,
    result: Result<check::Checked, String>,
}

#[derive(Default)]
struct Server {
    ready: Vec<Ready>,
    file: config::Offers,
    /// Per player slot (1..=32; 0 unused).
    clients: Vec<Client>,
    msg_id: u8,
    /// Check results by package path.
    cache: HashMap<PathBuf, Cached>,
    /// Offers already logged as unsendable this map (no `sv_downloadurl`, a URL too long).
    logged: Vec<String>,
}

static STATE: Global<RefCell<Option<Server>>> = Global(RefCell::new(None));

fn with<R>(f: impl FnOnce(&mut Server) -> R) -> R {
    let mut s = STATE.0.borrow_mut();
    f(s.get_or_insert_with(Server::default))
}

fn slot(e: *mut edict_t) -> Option<usize> {
    if e.is_null() {
        return None;
    }
    let i = engine().index_of(e);
    (1..=64).contains(&i).then_some(i as usize)
}

fn client<R>(e: *mut edict_t, f: impl FnOnce(&mut Client) -> R) -> Option<R> {
    let i = slot(e)?;
    with(|s| {
        if s.clients.len() <= i {
            s.clients.resize_with(i + 1, Client::default);
        }
        Some(f(&mut s.clients[i]))
    })
}

fn log(msg: &str) {
    engine().print(&format!("[ts] tspkg: {msg}\n"));
}

fn name_of(e: *mut edict_t) -> String {
    String::from_utf8_lossy(&crate::rules::netname(e)).into_owned()
}

/// Whether `e` hooks `TSPkg`: a port client (not a bot or HLTV) whose `_tsrs` version is at
/// least `port::TSPKG_SINCE`, or any readable version with `ts_dev_tspkg 1` and `sv_cheats 1`.
pub fn hooks_tspkg(e: *mut edict_t) -> bool {
    crate::gameplay::with_connecting_client(e, |c| match c {
        crate::gameplay::ConnectingClient::Port(v) => port::parse_userinfo_version(v).is_some_and(version_hooks_tspkg),
        _ => false,
    })
}

/// [`hooks_tspkg`] for a version.
pub fn version_hooks_tspkg(v: [i16; 3]) -> bool {
    port::hooks_tspkg(v) || (cvars::dev_cheats() && cvars::on(CVAR_DEV))
}

/// Reads `plugin_offers.toml` and checks its packages: at each map start (`rules::install`) and
/// on `sv_plugin_offers reload`.
pub fn map_start() {
    let dir = PathBuf::from(engine().game_dir());
    let path = dir.join(config::FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            log(&format!("{}: {e}; no offers", config::FILE));
            String::new()
        }
    };
    let (file, errors) = config::parse(&text);
    for e in errors {
        log(&e);
    }
    let roots = plugin_pkg::builtin_roots();
    with(|s| {
        s.logged.clear();
        s.ready.clear();
        for offer in &file.offers {
            let local = offer.local.split('/').fold(dir.clone(), |p, seg| p.join(seg));
            let meta = std::fs::metadata(&local).ok();
            let (size, modified) = (meta.as_ref().map_or(0, |m| m.len()), meta.as_ref().and_then(|m| m.modified().ok()));
            let result = match s.cache.get(&local) {
                Some(c) if meta.is_some() && (c.size, c.modified) == (size, modified) && c.offer == *offer => c.result.clone(),
                _ => {
                    let t = std::time::Instant::now();
                    let r = check::check(&dir, offer, &roots);
                    if r.is_ok() {
                        log(&format!("checked {} {} in {:.2} s", offer.id, offer.version, t.elapsed().as_secs_f32()));
                    }
                    s.cache.insert(local, Cached { size, modified, offer: offer.clone(), result: r.clone() });
                    r
                }
            };
            match result {
                Ok(checked) => {
                    log(&format!(
                        "offering {} {} ({}, {} bytes{})",
                        offer.id,
                        offer.version,
                        checked.signer,
                        offer.size,
                        if offer.required { ", required" } else { "" }
                    ));
                    s.ready.push(Ready { offer: offer.clone(), checked });
                }
                Err(why) => log(&format!("not offering {} {}: {why}", offer.id, offer.version)),
            }
        }
        s.file = file;
    });
}

/// The `TSPkg` op 1 for `o`, with its URL resolved; `Err` (for the log) when it can't be sent.
fn offer_message(o: &config::Offer) -> Result<tspkg::Message, String> {
    let (url, fastdl) = match &o.url {
        config::Url::Https(u) => (u.clone(), false),
        config::Url::Relative(rel) => {
            let base = cvars::s(c"sv_downloadurl");
            let base = base.trim().trim_end_matches('/');
            if base.is_empty() {
                return Err("its url is relative and sv_downloadurl is empty".into());
            }
            if !base.starts_with("http://") && !base.starts_with("https://") {
                return Err(format!("sv_downloadurl {base:?} isn't http:// or https://"));
            }
            (format!("{base}/{rel}"), true)
        }
    };
    let flags = if o.required { tspkg::FLAG_REQUIRED } else { 0 } | if fastdl { tspkg::FLAG_FASTDL } else { 0 };
    let m = tspkg::Message::Offer(tspkg::Offer {
        id: o.id.clone(),
        version: o.version.to_string(),
        size: o.size,
        sha256: o.sha256.0,
        flags,
        url,
        note: o.note.clone(),
    });
    m.body().map_err(|e| e.to_string())?;
    Ok(m)
}

/// Sends `m` to `e` as its chunks.
fn send(s: &mut Server, e: *mut edict_t, m: &tspkg::Message) -> bool {
    s.msg_id = s.msg_id.wrapping_add(1);
    match m.encode(s.msg_id) {
        Ok(chunks) => {
            for c in chunks {
                c.iter().fold(crate::messages::tspkg_one(e), |w, &b| w.byte(i32::from(b))).end();
            }
            true
        }
        Err(err) => {
            log(&format!("can't send to {}: {err}", name_of(e)));
            false
        }
    }
}

/// Offers `ready` to `e` (which hooks `TSPkg`) and records it: true when it went out.
fn send_offer(s: &mut Server, e: *mut edict_t, index: usize) -> bool {
    let o = &s.ready[index].offer;
    let m = match offer_message(o) {
        Ok(m) => m,
        Err(why) => {
            let key = format!("{} {}", o.id, o.version);
            if !s.logged.contains(&key) {
                log(&format!("can't offer {key}: {why}"));
                s.logged.push(key);
            }
            return false;
        }
    };
    let (id, sent) = (o.id.clone(), Sent { version: o.version.to_string(), required: o.required });
    if send(s, e, &m) {
        log(&format!("offered {id} {} to {}", sent.version, name_of(e)));
        let i = slot(e).unwrap_or(0);
        if s.clients.len() <= i {
            s.clients.resize_with(i + 1, Client::default);
        }
        s.clients[i].sent.insert(id, sent);
        return true;
    }
    false
}

/// The index of `id` among the offers that passed this map's checks.
pub fn ready_index(id: &str) -> Option<usize> {
    with(|s| s.ready.iter().position(|r| r.offer.id == id))
}

/// Offers the checked offer `index` ([`ready_index`]) to `e`, which hooks `TSPkg`, as
/// `sv_plugin_offer` does: true when it went out (the plugin API's `offer_client_plugin`).
pub fn offer(e: *mut edict_t, index: usize) -> bool {
    with(|s| index < s.ready.len() && send_offer(s, e, index))
}

/// The version of `id` that `e`'s client has loaded, as far as the server knows
/// ([`Client::loaded_version`]; the plugin API's `client_plugin_version`).
pub fn loaded_version(e: *mut edict_t, id: &str) -> Option<String> {
    client(e, |c| c.loaded_version(id).map(str::to_owned)).flatten()
}

/// `ClientConnect`, once the client is let in: a new connection (or the reconnect of a map
/// change) starts with no offers, answers or notice; `notice` is the op 5 the version check
/// asked for.
pub fn client_connect(e: *mut edict_t, notice: Option<PortNotice>) {
    client(e, |c| {
        *c = Client::default();
        c.notice = notice;
    });
}

pub fn client_disconnect(e: *mut edict_t) {
    client(e, |c| *c = Client::default());
}

/// `ClientPutInServer`: op 5 and the offers are due, at the player's first `PlayerPostThink`
/// ([`client_ready`]). Not now: FWGS's server drops user messages to a client that isn't
/// spawned yet (`SV_Multicast`, engine/server/sv_game.c: `cl->state != cs_spawned &&
/// usermessage`), and at `ClientPutInServer` it isn't; GoldSrc queues them, but both run
/// `PlayerPostThink` only for spawned clients (issue #317).
pub fn client_put_in_server(e: *mut edict_t) {
    if e.is_null() || e.v().flags & FL_FAKECLIENT as i32 != 0 {
        return;
    }
    client(e, |c| c.put_pending = true);
}

/// `PlayerPostThink`: the first one after `ClientPutInServer` sends what it left due.
pub fn client_ready(e: *mut edict_t) {
    if client(e, |c| std::mem::replace(&mut c.put_pending, false)) == Some(true) {
        send_put_in_server(e);
    }
}

/// Op 5 if the version check asked for it, then every offer, to a client that hooks `TSPkg`.
/// Nothing is sent to any other client (§12).
fn send_put_in_server(e: *mut edict_t) {
    if e.is_null() || e.v().flags & FL_FAKECLIENT as i32 != 0 {
        return;
    }
    let Some(i) = slot(e) else { return };
    let hooks = hooks_tspkg(e);
    let now = engine().time();
    let check_time = cvars::f(CVAR_CHECK_TIME).max(0.0);
    with(|s| {
        if s.clients.len() <= i {
            s.clients.resize_with(i + 1, Client::default);
        }
        if let Some(n) = s.clients[i].notice.clone() {
            // The kick is due even if the client no longer counts as hooking `TSPkg` (the dev
            // aid turned off since it connected): it was let in only to be told.
            let m = tspkg::Message::PortVersion { requirement: n.requirement.clone(), url: n.url.clone() };
            if hooks && send(s, e, &m) {
                log(&format!("port_version {} to {}; kick in {check_time} s unless it leaves", n.requirement, name_of(e)));
            }
            if let Some(n) = s.clients[i].notice.as_mut() {
                n.kick_at = Some(now + check_time);
            }
        }
        if !hooks {
            return;
        }
        s.clients[i].enforce.ready_at = Some(now);
        for k in 0..s.ready.len() {
            send_offer(s, e, k);
        }
    });
}

/// [`enforce::step`] for every client the offers went to, at `now`.
fn enforce_all(now: f32) {
    let times = enforce::Times { check: cvars::f(CVAR_CHECK_TIME).max(0.0), download: cvars::f(CVAR_DOWNLOAD_TIME).max(0.0) };
    let required_kick = cvars::on(CVAR_REQUIRED_KICK);
    let busy = with(|s| s.clients.iter().any(|c| c.enforce.ready_at.is_some() && !c.enforce.kicked));
    if !busy {
        return;
    }
    let mut kicks = Vec::new();
    with(|s| {
        let named: std::collections::BTreeMap<String, enforce::Named> = s
            .ready
            .iter()
            .map(|r| (r.offer.id.clone(), enforce::Named { name: r.checked.name.clone(), homepage: r.checked.homepage.clone() }))
            .collect();
        for i in 0..s.clients.len() {
            if s.clients[i].enforce.ready_at.is_none() || s.clients[i].enforce.kicked {
                continue;
            }
            let file = std::mem::take(&mut s.file);
            let acts = enforce::step(&mut s.clients[i], now, &file, &named, times, required_kick);
            s.file = file;
            if acts.is_empty() {
                continue;
            }
            let e = engine().player(i as i32);
            if e.is_null() {
                continue;
            }
            let who = name_of(e);
            for a in acts {
                match a {
                    enforce::Act::Log(line) => log(&format!("{who}: {line}")),
                    enforce::Act::Send(m) => {
                        if send(s, e, &m) {
                            log(&format!("{} to {who}", describe(&m)));
                        }
                    }
                    enforce::Act::Kick(reason) => kicks.push((e, reason)),
                }
            }
        }
    });
    for (e, reason) in kicks {
        kick(e, &reason);
    }
}

/// An op 3 or 4 for the log.
fn describe(m: &tspkg::Message) -> String {
    match m {
        tspkg::Message::Require { id, requirement, flags } => {
            format!("require {id} {requirement}{}", if flags & tspkg::FLAG_REQUIRED != 0 { " (required)" } else { "" })
        }
        tspkg::Message::Block { id, requirement, action, .. } => {
            format!("block {id} {requirement} ({})", if *action == tspkg::BlockAction::Kick { "kick" } else { "suspend" })
        }
        other => format!("op {}", other.op()),
    }
}

/// `kick #<userid> "<reason>"`, with the reason cleaned for the console and cut to fit.
fn kick(e: *mut edict_t, reason: &str) {
    let eng = engine();
    let reason = crate::rules::client_version::clean_reason(reason);
    log(&format!("kicking {}: {reason}", name_of(e)));
    eng.server_command(&format!("kick #{} \"{reason}\"\n", eng.user_id(e)));
}

/// Kicks clients whose op 5 notice ran out, and enforces the plugin rules (`StartFrame`).
pub fn start_frame() {
    let now = engine().time();
    enforce_all(now);
    let due: Vec<(usize, String)> = with(|s| {
        s.clients
            .iter_mut()
            .enumerate()
            .filter_map(|(i, c)| {
                let n = c.notice.as_mut()?;
                if n.kick_at.is_some_and(|t| now >= t) {
                    n.kick_at = None;
                    Some((i, n.reason.clone()))
                } else {
                    None
                }
            })
            .collect()
    });
    for (i, reason) in due {
        let e = engine().player(i as i32);
        if !e.is_null() {
            kick(e, &reason);
        }
    }
}

/// The `tspkg` client command (§6.5). True when it was `tspkg` (handled or ignored).
pub fn client_command(e: *mut edict_t, args: &Args) -> bool {
    if args.str(0) != tspkg::REPLY_COMMAND {
        return false;
    }
    let argv: Vec<String> = (1..args.argc()).map(|i| args.str(i).to_owned()).collect();
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let now = engine().time();
    let required_kick = cvars::on(CVAR_REQUIRED_KICK);
    let hooks = hooks_tspkg(e);
    let Some(outcome) = client(e, |c| c.command(now, &argv, required_kick, hooks)) else { return true };
    let who = name_of(e);
    match outcome {
        Outcome::Ignored(Some(why)) => log(&format!("{who}: {why}")),
        Outcome::Ignored(None) => {}
        Outcome::List { complete } => {
            if complete {
                let plugins = client(e, |c| c.plugins.clone()).unwrap_or_default();
                let list = plugins.iter().map(|(i, v)| format!("{i}@{v}")).collect::<Vec<_>>().join(" ");
                log(&format!("{who}: plugins: {}", if list.is_empty() { "none" } else { &list }));
                crate::plugins::client_plugin_list(e, &plugins);
            }
        }
        Outcome::Answer { id, version, state, reason, kick: k } => {
            log(&format!("{who}: {id} {version} {state}{}{reason}", if reason.is_empty() { "" } else { " " }));
            // Before a kick, so plugins hear the refusal that caused it.
            crate::plugins::client_plugin_state(e, &id, &version, crate::plugin_api::client_plugins::state_code(state), &reason);
            if k {
                let (name, homepage, version) = with(|s| {
                    s.ready
                        .iter()
                        .find(|r| r.offer.id == id)
                        .map(|r| (r.checked.name.clone(), r.checked.homepage.clone(), r.offer.version.to_string()))
                        .unwrap_or((id.clone(), None, version.clone()))
                });
                kick(e, &clients::required_kick_reason(&name, &version, homepage.as_deref()));
            }
        }
    }
    true
}

/// The players a `sv_plugin_*` target names: `#<userid>`, a name, or `all`.
fn targets(target: &str) -> Vec<*mut edict_t> {
    let eng = engine();
    let found: Vec<_> = crate::rules::players()
        .into_iter()
        .filter(|&e| e.v().flags & FL_FAKECLIENT as i32 == 0)
        .filter(|&e| match target.strip_prefix('#') {
            _ if target == "all" => true,
            Some(uid) => uid.parse() == Ok(eng.user_id(e)),
            None => name_of(e) == target,
        })
        .collect();
    if found.is_empty() && target != "all" {
        log(&format!("no player {target:?}"));
    }
    found
}

fn usage(cmd: &str) {
    engine().print(&format!("usage: {cmd} <#userid|name|all> <id>\n"));
}

/// `sv_plugin_offer <#userid|name|all> <id>` (port addition): offers `id` now.
pub extern "C" fn cmd_offer() {
    let eng = engine();
    if eng.cmd_argc() < 3 {
        return usage("sv_plugin_offer");
    }
    let (target, id) = (eng.cmd_argv(1).to_string_lossy().into_owned(), eng.cmd_argv(2).to_string_lossy().into_owned());
    let Some(index) = with(|s| s.ready.iter().position(|r| r.offer.id == id)) else {
        return log(&format!("sv_plugin_offer: no checked offer for {id:?} (sv_plugin_offers lists them)"));
    };
    for e in targets(&target) {
        if hooks_tspkg(e) {
            with(|s| send_offer(s, e, index));
        } else {
            log(&format!(
                "sv_plugin_offer: {} doesn't hook TSPkg (not a port client from {}.{}.{} on)",
                name_of(e),
                port::TSPKG_SINCE[0],
                port::TSPKG_SINCE[1],
                port::TSPKG_SINCE[2]
            ));
        }
    }
}

/// `sv_plugin_withdraw <#userid|name|all> <id>` (port addition): op 2 for an offer sent earlier.
pub extern "C" fn cmd_withdraw() {
    let eng = engine();
    if eng.cmd_argc() < 3 {
        return usage("sv_plugin_withdraw");
    }
    let (target, id) = (eng.cmd_argv(1).to_string_lossy().into_owned(), eng.cmd_argv(2).to_string_lossy().into_owned());
    for e in targets(&target) {
        let Some(i) = slot(e) else { continue };
        with(|s| {
            let Some(sent) = s.clients.get_mut(i).and_then(|c| c.sent.remove(&id)) else { return };
            let m = tspkg::Message::Withdraw { id: id.clone(), version: sent.version.clone() };
            if send(s, e, &m) {
                log(&format!("withdrew {id} {} from {}", sent.version, name_of(e)));
            }
        });
    }
}

/// `sv_plugin_offers [reload]` (port addition): lists the offers and whether they passed their
/// checks, the blocks and the policy; `reload` reads the file again first.
pub extern "C" fn cmd_offers() {
    let eng = engine();
    if eng.cmd_argc() > 1 && eng.cmd_argv(1).to_bytes() == b"reload" {
        map_start();
    }
    let text = with(|s| {
        let mut t = String::new();
        for o in &s.file.offers {
            let ready = s.ready.iter().find(|r| r.offer.id == o.id);
            let url = match &o.url {
                config::Url::Relative(u) => format!("<sv_downloadurl>/{u}"),
                config::Url::Https(u) => u.clone(),
            };
            t.push_str(&format!(
                "  {} {} {} bytes{} match {:?}: {}\n",
                o.id,
                o.version,
                o.size,
                if o.required { " required" } else { "" },
                o.match_rule,
                match ready {
                    Some(r) => format!("offered ({}), {url}", r.checked.signer),
                    None => "not offered (see the log)".into(),
                }
            ));
        }
        for b in &s.file.blocks {
            t.push_str(&format!("  block {:?} {} {:?} {:?}\n", b.id, b.versions, b.action, b.reason));
        }
        t.push_str(&format!("  unlisted {:?}\n", s.file.unlisted));
        t
    });
    eng.print(&format!("[ts] tspkg: {} ({}):\n{text}", config::FILE, Path::new(&eng.game_dir()).join(config::FILE).display()));
}

pub fn register_server_commands() {
    let e = engine();
    e.add_server_command(c"sv_plugin_offer", cmd_offer);
    e.add_server_command(c"sv_plugin_withdraw", cmd_withdraw);
    e.add_server_command(c"sv_plugin_offers", cmd_offers);
}
