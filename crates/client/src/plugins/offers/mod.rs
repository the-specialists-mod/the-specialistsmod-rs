//! Server offers of client plugins (port addition, issue #299; client-plugins.md §6.3–§6.5, §7.2,
//! §7.3, §9, §13.1): what the client does with a server's `TSPkg`.
//!
//! An offer (op 1) is checked ([`flow::Connection::admit`], `cl_plugins_download`, the URL's
//! transport), then its manifest, signature and countersignature are fetched on a worker thread
//! ([`fetch`]) and judged ([`flow::assess`]): `failed`, `unsupported` or `blocked` at once, `have`
//! when the same manifest is approved here, otherwise a card ([`card`]) and `offered`. Only a click
//! on the card's approving button downloads anything: into `cl_plugins/staging/`, on a worker,
//! checked against the offer's size and SHA-256 and the card's manifest, unpacked atomically and
//! verified again. The approval goes into the store and the gate's in-memory copy, and the plugin
//! loads at once (`loaded`, through [`crate::ext::load_one`]) unless its scope means later
//! (`installed`) or an older version of it is loaded (`restart`, and a card offering to restart
//! through [`crate::relaunch::restart`]). Op 5 shows the port version card, whose button also
//! restarts through `relaunch` (a port update always goes through the launcher, which asks).
//!
//! Ops 3 `require` and 4 `block` ([`enforce`], §13.3, issue #302) suspend a blocked plugin until
//! the player leaves the server (`ext::suspend`), or offer the restart card when an approved
//! version the server needs is installed but another is loaded.
//!
//! Nothing here approves without a click on a card (§7.3): no console command, and nothing a
//! server sends. `cl_plugins_cancel` (port addition) only stops downloads.
//!
//! The cards are drawn in game (see [`card`] for why not SDL windows); a debug hook answers them
//! in `--features e2e` builds ([`e2e`], `cargo xtask tspkg`).

pub mod card;
#[cfg(feature = "e2e")]
mod e2e;
pub mod enforce;
pub mod fetch;
pub mod flow;

use super::click::Arming;
use super::startup::{Decision, Item};
use card::{Action, Card, Clicks};
use fetch::{Done, InstallJob, Progress};
use flow::{Admit, Assessment, CardInfo, Connection, Transport};
use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
use plugin_pkg::{Scope, Verdict, Verification};
use shared::protocol::tspkg::{BlockAction, Message, Offer, PORT_ID, Reply, State as Answer, list_commands};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, channel};

fn say(line: &str) {
    engine().print(&format!("[ts] tspkg: {line}\n"));
}

enum Stage {
    Fetching,
    /// Waiting for (or showing) its card.
    Card(Box<CardInfo>),
    Installing {
        progress: Progress,
        scope: Scope,
        info: Box<CardInfo>,
    },
}

struct Slot {
    key: u32,
    offer: Offer,
    transport: Transport,
    stage: Stage,
}

/// A card waiting to be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    Offer(u32),
    Port { requirement: String, url: String },
    Restart { id: String, version: String, name: String },
}

struct Open {
    card: Card,
    from: Pending,
    arming: Arming,
    clicks: Clicks,
}

struct Conn {
    server: String,
    seen: Connection,
    slots: Vec<Slot>,
    port_card: bool,
    /// Ids whose restart card was queued this connection (once each).
    restarts: std::collections::BTreeSet<String>,
}

struct Offers {
    conn: Option<Conn>,
    queue: VecDeque<Pending>,
    open: Option<Open>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    next_key: u32,
    shift: bool,
}

static OFFERS: Global<RefCell<Option<Offers>>> = Global(RefCell::new(None));

/// The closure must not call back into this module.
fn with<R>(f: impl FnOnce(&mut Offers) -> R) -> R {
    let mut o = OFFERS.0.borrow_mut();
    f(o.get_or_insert_with(|| {
        let (tx, rx) = channel();
        Offers { conn: None, queue: VecDeque::new(), open: None, tx, rx, next_key: 1, shift: false }
    }))
}

fn server_address() -> Option<String> {
    engine().server_address()
}

fn server_line(address: &str) -> String {
    card::server_text(&crate::hud::with(|h| h.server_name.clone()), address)
}

fn cvar_on(name: &std::ffi::CStr, default: bool) -> bool {
    let c = Cvar::find(name);
    if c.is_null() { default } else { c.value() != 0.0 }
}

/// Sends `tspkg <id> <version> <state> [reason]` and logs it.
fn reply(id: &str, version: &str, state: Answer, reason: &str) {
    let r = Reply::State { id: id.into(), version: version.into(), state, reason: reason.into() };
    match r.command() {
        Ok(cmd) => {
            say(&format!("{id} {version}: {}{}", state.name(), if reason.is_empty() { String::new() } else { format!(" ({reason})") }));
            engine().server_cmd(&cmd);
        }
        Err(e) => say(&format!("can't answer for {id} {version}: {e}")),
    }
}

// ---- engine entry points ----------------------------------------------------------------------

/// `HUD_Init`: `cl_plugins_cancel` (port addition): stops every download. It can't approve.
pub fn init() {
    engine().add_command(c"cl_plugins_cancel", cmd_cancel);
}

extern "C" fn cmd_cancel() {
    let n = with(|o| {
        let mut n = 0;
        for s in o.conn.iter().flat_map(|c| &c.slots) {
            if let Stage::Installing { progress, .. } = &s.stage {
                progress.cancel.store(true, Ordering::Relaxed);
                n += 1;
            }
        }
        n
    });
    say(&format!("cancelling {n} download(s)"));
}

/// A `TSPkg` body (the hook in `crate::hud::tspkg`).
pub fn on_message(m: &Message) {
    let Some(server) = server_address() else { return };
    connection(&server);
    send_list();
    let server = server.as_str();
    match m {
        Message::Offer(o) => receive(o),
        Message::Withdraw { id, version } => withdraw(id, version),
        // Once per connection: a map change sends it again.
        Message::PortVersion { requirement, url } => with(|o| {
            if let Some(c) = o.conn.as_mut()
                && !std::mem::replace(&mut c.port_card, true)
            {
                o.queue.push_back(Pending::Port { requirement: requirement.clone(), url: url.clone() });
            }
        }),
        Message::Require { id, requirement, .. } => require(server, id, requirement),
        Message::Block { id, requirement, action, reason } => block(id, requirement, *action, reason),
    }
}

/// The HUD has the server's version (`SrvSett` 5–7, at each map): a new game, so the plugin list
/// goes again (the server forgets it at the map change's reconnect). A server whose version
/// understands `tspkg` gets it now, so a server with blocks or a policy but nothing to offer
/// learns it too; any other gets it after its first `TSPkg` (builds between releases report
/// the last release's version).
pub fn new_game(server_version: [i16; 3]) {
    with(|o| {
        if let Some(c) = o.conn.as_mut() {
            c.seen.listed = false;
        }
    });
    if !shared::protocol::port::hooks_tspkg(server_version) {
        return;
    }
    let Some(server) = server_address() else { return };
    connection(&server);
    send_list();
}

/// Op 4 (§13.3): suspends the loaded plugins it names until the player leaves this server, or
/// says the server disconnects players who use them.
fn block(id: &str, requirement: &str, action: BlockAction, reason: &str) {
    let req = match enforce::requirement(requirement) {
        Ok(r) => r,
        Err(e) => return say(&format!("a block of {id} the client can't read: {e}")),
    };
    let loaded = super::loaded_plugins();
    let hits = enforce::blocked(&loaded, id, &req);
    if hits.is_empty() {
        return say(&format!("the server blocks {id} {requirement}, which isn't loaded"));
    }
    for p in hits {
        if action == BlockAction::Kick {
            say(&format!("this server doesn't allow {} {} and disconnects players who use it", p.id, p.version));
            continue;
        }
        let why = if reason.is_empty() { "blocked by this server" } else { reason };
        let now = crate::ext::suspend(&p.ext, why);
        say(&format!("{} {} suspended while on this server: {why}{}", p.id, p.version, if now { "" } else { " (already)" }));
        reply(&p.id, &p.version, Answer::Suspended, reason);
    }
}

/// Op 3 (§13.3): when the loaded version doesn't fit and an approved one that does is installed,
/// `restart` and the restart card; when none is, the server's offer brings the update card.
fn require(server: &str, id: &str, requirement: &str) {
    let req = match enforce::requirement(requirement) {
        Ok(r) => r,
        Err(e) => return say(&format!("a requirement for {id} the client can't read: {e}")),
    };
    // An offer of it is being handled: its card (or its restart card) does the rest.
    if with(|o| o.conn.as_ref().is_some_and(|c| c.slots.iter().any(|s| s.offer.id == id))) {
        return;
    }
    let loaded = super::loaded_plugins();
    let decision = super::with(|s| enforce::require(&loaded, id, &req, &s.store, s.opts.platform, server, &s.dirs.cl_plugins));
    match decision {
        None | Some(enforce::Require::Nothing) => {}
        Some(enforce::Require::AwaitOffer { loaded }) => {
            say(&format!("{id} {loaded} is loaded and this server needs {requirement}: accept its offer to update"))
        }
        Some(enforce::Require::Restart(v)) => {
            if !with(|o| o.conn.as_mut().is_some_and(|c| c.restarts.insert(id.to_owned()))) {
                return;
            }
            let version = v.to_string();
            reply(id, &version, Answer::Restart, &format!("needs {requirement}"));
            with(|o| o.queue.push_back(Pending::Restart { id: id.to_owned(), version, name: id.to_owned() }));
        }
    }
}

/// Starts a new connection's state when the server changed (or there was none): the old
/// connection's downloads stop and its cards go.
fn connection(server: &str) {
    let stale = with(|o| o.conn.as_ref().is_some_and(|c| c.server != server) || o.conn.is_none());
    if stale {
        reset();
        with(|o| {
            o.conn = Some(Conn {
                server: server.to_owned(),
                seen: Connection::default(),
                slots: Vec::new(),
                port_card: false,
                restarts: Default::default(),
            })
        });
    }
}

/// Drops the connection: downloads are cancelled (their staging files go with them), cards close.
fn reset() {
    with(|o| {
        if let Some(c) = o.conn.take() {
            for s in c.slots {
                if let Stage::Installing { progress, .. } = s.stage {
                    progress.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        o.queue.clear();
        o.open = None;
    });
}

/// `tspkg list`, once per map, after the first `TSPkg` or the server's version ([`new_game`], §13.2): every
/// loaded plugin.
fn send_list() {
    if with(|o| o.conn.as_mut().map(|c| std::mem::replace(&mut c.seen.listed, true))) != Some(false) {
        return;
    }
    let list: Vec<(String, String)> = super::loaded_plugins().into_iter().map(|p| (p.id, p.version)).collect();
    match list_commands(&list) {
        Ok(cmds) => cmds.iter().for_each(|c| engine().server_cmd(c)),
        Err(e) => say(&format!("can't list the loaded plugins: {e}")),
    }
}

fn receive(o: &Offer) {
    if !cvar_on(c"cl_plugins_download", true) {
        say("cl_plugins_download is 0: ignored");
        return;
    }
    let enabled = super::with(|s| s.opts.enabled);
    match enabled {
        None => return reply(&o.id, &o.version, Answer::Unsupported, "client plugins aren't available"),
        Some(false) => {
            say("cl_plugins is 0: ignored");
            return;
        }
        Some(true) => {}
    }
    let admit = with(|w| {
        let c = w.conn.as_mut()?;
        let busy = c.slots.iter().any(|s| s.offer.id == o.id);
        Some(c.seen.admit(&o.id, busy))
    });
    match admit {
        None | Some(Admit::Busy) => return,
        Some(Admit::Flood) => return say(&format!("more than {} offers from this server; {} ignored", flow::MAX_OFFERS, o.id)),
        Some(Admit::Refused) => return reply(&o.id, &o.version, Answer::Denied, ""),
        Some(Admit::Fetch) => {}
    }
    let t = match flow::transport(o) {
        Ok(t) => t,
        Err(e) => return reply(&o.id, &o.version, Answer::Failed, &e),
    };
    let (key, tx) = with(|w| {
        let key = w.next_key;
        w.next_key += 1;
        if let Some(c) = &mut w.conn {
            c.slots.push(Slot { key, offer: o.clone(), transport: t.clone(), stage: Stage::Fetching });
        }
        (key, w.tx.clone())
    });
    fetch::spawn(&tx, move || Done::Manifest { key, result: fetch::fetch_manifest(&t) });
}

fn withdraw(id: &str, version: &str) {
    let gone = with(|o| {
        let c = o.conn.as_mut()?;
        let i = c.slots.iter().position(|s| s.offer.id == id && s.offer.version == version)?;
        let s = c.slots.remove(i);
        if let Stage::Installing { progress, .. } = &s.stage {
            progress.cancel.store(true, Ordering::Relaxed);
        }
        o.queue.retain(|p| *p != Pending::Offer(s.key));
        if o.open.as_ref().is_some_and(|op| op.from == Pending::Offer(s.key)) {
            o.open = None;
        }
        Some(())
    });
    if gone.is_some() {
        say(&format!("the server withdrew {id} {version}"));
    }
}

/// `HUD_Frame`: the connection check, the workers' results, the next card.
pub fn frame() {
    if OFFERS.0.borrow().is_none() {
        return;
    }
    let server = server_address();
    if with(|o| o.conn.as_ref().map(|c| c.server.clone())) != server {
        if server.is_none() {
            reset();
        } else if let Some(s) = &server {
            connection(s);
        }
    }
    while let Some(done) = with(|o| o.rx.try_recv().ok()) {
        match done {
            Done::Manifest { key, result } => manifest_done(key, result),
            Done::Install { key, result } => install_done(key, result),
        }
    }
    // The plugins panel holds the screen: queued cards wait until it closes, so none appears
    // under a click meant for the panel.
    if with(|o| o.open.is_none()) && !super::menu::is_open() {
        next_card();
    }
}

fn slot_offer(key: u32) -> Option<(Offer, Transport)> {
    with(|o| o.conn.as_ref()?.slots.iter().find(|s| s.key == key).map(|s| (s.offer.clone(), s.transport.clone())))
}

fn drop_slot(key: u32) {
    with(|o| {
        if let Some(c) = &mut o.conn {
            c.slots.retain(|s| s.key != key);
        }
    });
}

fn manifest_done(key: u32, result: Result<flow::Fetched, String>) {
    let Some((o, t)) = slot_offer(key) else { return };
    let Some(server) = with(|w| w.conn.as_ref().map(|c| c.server.clone())) else { return };
    let fetched = match result {
        Ok(f) => f,
        Err(e) => {
            drop_slot(key);
            return reply(&o.id, &o.version, Answer::Failed, &e);
        }
    };
    let allow_unsigned = cvar_on(c"cl_plugins_allow_unsigned", false);
    let roots = plugin_pkg::builtin_roots();
    let assessment = super::with(|s| {
        let cx = flow::Ctx {
            platform: s.opts.platform,
            port: &s.opts.port,
            api: s.opts.api,
            server: &server,
            allow_unsigned,
            roots: &roots,
            store: &s.store,
            cl_plugins: &s.dirs.cl_plugins,
        };
        flow::assess(&o, &t, &fetched, &cx)
    });
    match assessment {
        None => {
            drop_slot(key);
            reply(&o.id, &o.version, Answer::Unsupported, "client plugins aren't available");
        }
        Some(Assessment::Answer(state, why)) => {
            drop_slot(key);
            reply(&o.id, &o.version, state, &why);
        }
        Some(Assessment::Have) => {
            drop_slot(key);
            reply(&o.id, &o.version, Answer::Have, "");
        }
        Some(Assessment::Card(info)) => {
            with(|w| {
                if let Some(s) = w.conn.as_mut().and_then(|c| c.slots.iter_mut().find(|s| s.key == key)) {
                    s.stage = Stage::Card(info);
                    w.queue.push_back(Pending::Offer(key));
                }
            });
            reply(&o.id, &o.version, Answer::Offered, "");
        }
    }
}

/// Opens the next queued card (or answers it from the e2e hook).
fn next_card() {
    let Some(server) = with(|o| o.conn.as_ref().map(|c| c.server.clone())) else { return };
    let line = server_line(&server);
    let platform = super::with(|s| s.opts.platform);
    loop {
        let Some(p) = with(|o| o.queue.pop_front()) else { return };
        let card = match &p {
            Pending::Offer(key) => {
                let c = with(|o| {
                    let s = o.conn.as_ref()?.slots.iter().find(|s| s.key == *key)?;
                    let Stage::Card(info) = &s.stage else { return None };
                    Some(card::offer(&s.offer, info, platform?, &line))
                });
                match c {
                    Some(c) => c,
                    None => continue,
                }
            }
            Pending::Port { requirement, url } => card::port(&line, requirement, url, shared::protocol::port::VERSION),
            Pending::Restart { id, version, name } => card::restart(&line, name, version, id),
        };
        say(&format!("card: {}", card.summary()));
        #[cfg(feature = "e2e")]
        if let Some(a) = e2e::answer(&card) {
            act(&p, &card, a);
            continue;
        }
        with(|o| o.open = Some(Open { card, from: p, arming: Arming::now(), clicks: Clicks::default() }));
        return;
    }
}

/// A card's answer.
fn act(from: &Pending, card: &Card, a: Action) {
    say(&format!("card {} {}: {}", card.kind.name(), card.id, a.name()));
    match from {
        Pending::Offer(key) => offer_answer(*key, a),
        Pending::Port { requirement, url } => {
            let ours = shared::protocol::port::VERSION;
            if a == Action::Update {
                reply(PORT_ID, ours, Answer::Restart, "");
                let reason = crate::relaunch::Reason::Port { requirement: requirement.clone(), url: url.clone() };
                if let Err(e) = crate::relaunch::restart(reason) {
                    engine().print(&format!("[ts] relaunch: {e}\n"));
                }
            } else {
                reply(PORT_ID, ours, Answer::Denied, "");
            }
        }
        Pending::Restart { id, version, .. } => {
            if a == Action::Restart
                && let Err(e) = crate::relaunch::restart(crate::relaunch::Reason::Plugin { plugin: format!("{id}@{version}") })
            {
                engine().print(&format!("[ts] relaunch: {e}\n"));
            }
        }
    }
}

fn offer_answer(key: u32, a: Action) {
    let Some((o, t)) = slot_offer(key) else { return };
    let Some(server) = with(|w| w.conn.as_ref().map(|c| c.server.clone())) else { return };
    let info = with(|w| {
        let s = w.conn.as_mut()?.slots.iter_mut().find(|s| s.key == key)?;
        match std::mem::replace(&mut s.stage, Stage::Fetching) {
            Stage::Card(info) => Some(info),
            other => {
                s.stage = other;
                None
            }
        }
    });
    let Some(info) = info else { return };
    let scope = match a {
        Action::Install => Scope::Always,
        Action::ServerOnly => Scope::Server,
        Action::Never | Action::NeverEverywhere => {
            let everywhere = a == Action::NeverEverywhere;
            super::with(|s| {
                s.store.block(flow::blocking(&info.checked, s.opts.platform, &server, everywhere));
                save_store(s);
            });
            return refuse(key, &o);
        }
        _ => return refuse(key, &o),
    };
    let Some(job) = super::with(|s| {
        let dest = flow::version_dir(&s.dirs.cl_plugins, &o.id, &info.checked.manifest.version);
        InstallJob {
            url: o.url.clone(),
            plain_http: t.plain_http,
            size: o.size,
            sha256: o.sha256,
            manifest_sha256: info.checked.sha256,
            key: info.checked.key(),
            staging: s.dirs.cl_plugins.join(plugin_pkg::verify::STAGING_DIR).join(format!("{}-{}.tspkg.part", o.id, o.version)),
            reuse: info.installed.as_deref() == Some(dest.as_path()),
            dest,
            platform: s.opts.platform,
            roots: plugin_pkg::builtin_roots(),
            store: s.store.clone(),
        }
    }) else {
        return;
    };
    let progress = Progress::default();
    let (tx, p) = with(|w| {
        if let Some(s) = w.conn.as_mut().and_then(|c| c.slots.iter_mut().find(|s| s.key == key)) {
            s.stage = Stage::Installing { progress: progress.clone(), scope, info };
        }
        (w.tx.clone(), progress.clone())
    });
    if !job.reuse {
        reply(&o.id, &o.version, Answer::Downloading, "");
    }
    fetch::spawn(&tx, move || Done::Install { key, result: fetch::install(&job, &p).map(Box::new) });
}

/// "Not now" or "Never": `denied`, and not shown again this connection.
fn refuse(key: u32, o: &Offer) {
    with(|w| {
        if let Some(c) = &mut w.conn {
            c.seen.refused.insert(o.id.clone());
        }
    });
    drop_slot(key);
    reply(&o.id, &o.version, Answer::Denied, "");
}

fn save_store(s: &super::State) {
    if s.store_writable
        && let Err(e) = super::save_store(s)
    {
        super::say(&e);
    }
}

fn install_done(key: u32, result: Result<Box<Verification>, String>) {
    let slot = with(|w| {
        let c = w.conn.as_mut()?;
        let i = c.slots.iter().position(|s| s.key == key)?;
        Some((c.slots.remove(i), c.server.clone()))
    });
    let Some((slot, server)) = slot else {
        // The connection (or the offer) went while it ran: nothing may stay behind.
        if let Ok(v) = &result
            && v.base.starts_with(super::with(|s| s.dirs.cl_plugins.clone()).unwrap_or_default())
        {
            let _ = std::fs::remove_dir_all(&v.base);
        }
        return;
    };
    let o = &slot.offer;
    let Stage::Installing { scope, info, .. } = slot.stage else { return };
    let v = match result {
        Ok(v) => *v,
        Err(e) => return reply(&o.id, &o.version, Answer::Failed, &e),
    };
    let servers = if scope == Scope::Server { vec![server.clone()] } else { Vec::new() };
    let loaded = crate::ext::loaded();
    let approved = super::with(|s| approve(s, v, scope, servers, &server, &loaded));
    match approved {
        None | Some(Err(_)) => {
            let why = approved.and_then(Result::err).unwrap_or_else(|| "client plugins aren't available".into());
            reply(&o.id, &o.version, Answer::Failed, &why);
        }
        Some(Ok(Installed::Restart)) => {
            reply(&o.id, &o.version, Answer::Restart, "restart to finish");
            let name = info.checked.manifest.name.clone();
            with(|w| {
                if let Some(c) = w.conn.as_mut() {
                    c.restarts.insert(o.id.clone());
                }
                w.queue.push_back(Pending::Restart { id: o.id.clone(), version: o.version.clone(), name })
            });
        }
        Some(Ok(Installed::Later)) => reply(&o.id, &o.version, Answer::Installed, ""),
        Some(Ok(Installed::Load(c))) => {
            let ok = crate::ext::opener().is_some_and(|open| crate::ext::load_one(&c, open));
            crate::ext::refresh_server_wait();
            if ok {
                reply(&o.id, &o.version, Answer::Loaded, "");
            } else {
                reply(&o.id, &o.version, Answer::Failed, "load");
            }
        }
    }
}

enum Installed {
    /// Loads now: the candidate for the loader.
    Load(crate::ext::gate::Candidate),
    /// Its scope doesn't cover this server.
    Later,
    /// An older version is loaded: loads at the next start.
    Restart,
}

/// Writes the approval (store and gate) and decides whether the plugin loads now (§6.3 steps
/// 6–7, §9).
fn approve(
    s: &mut super::State,
    mut v: Verification,
    scope: Scope,
    servers: Vec<String>,
    server: &str,
    loaded: &[String],
) -> Result<Installed, String> {
    let servers = flow::servers_with(&s.store, &v, scope, servers);
    let entry = v.approval(&flow::source(server), scope, servers).ok_or("can't be approved as installed")?;
    s.store.allow(entry.clone());
    save_store(s);
    let id = v.id.clone();
    let library = v.library.clone().ok_or("no library for this platform")?;
    let applies = entry.applies_to(Some(server));
    v.entry = Some(entry);
    v.verdict = Verdict::Load;
    let base = v.base.clone();
    let old_loaded = loaded.contains(&id);
    // One version at a time (§4): the other versions go, except one that's loaded now (it goes
    // at the next start).
    let keep: Vec<PathBuf> =
        s.items.iter().filter(|it| it.candidate.id() == id && loaded.contains(&it.name)).filter_map(item_dir).collect();
    remove_other_versions(&s.dirs.cl_plugins.join(&id), &base, &keep);
    if old_loaded {
        return Ok(Installed::Restart);
    }
    s.items.retain(|it| it.candidate.id() != id);
    s.items.push(Item {
        candidate: plugin_pkg::Candidate::Package { dir: base },
        name: id.clone(),
        library: Some(library.clone()),
        found: super::startup::Found::of(&v),
        decision: Decision::Allowed { v: Box::new(v), fresh: true },
    });
    s.by_library = super::startup::by_library(&s.items);
    if !applies {
        return Ok(Installed::Later);
    }
    if s.server.is_none() {
        s.server = Some(server.to_owned());
    }
    let file = library.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Installed::Load(crate::ext::gate::Candidate { name: id, file, library }))
}

fn item_dir(it: &Item) -> Option<PathBuf> {
    match &it.candidate {
        plugin_pkg::Candidate::Package { dir } => Some(dir.clone()),
        plugin_pkg::Candidate::Manual { .. } => None,
    }
}

/// Removes every version directory of one plugin but `current` and `keep`.
fn remove_other_versions(id_dir: &Path, current: &Path, keep: &[PathBuf]) {
    for e in std::fs::read_dir(id_dir).into_iter().flatten().flatten() {
        let p = e.path();
        let hidden = e.file_name().to_string_lossy().starts_with('.');
        if !hidden && p != current && !keep.contains(&p) && e.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// At start, before verification (§4): unfinished downloads in `staging/`, directories an install
/// set aside (`.<version>.replaced`, `.<version>.partial`), and versions older than the approved
/// one of the same plugin when that one is installed. Nothing is loaded yet, so all can go.
pub fn clean_at_start(cl_plugins: &Path, store: &plugin_pkg::Store, platform: plugin_pkg::Platform) {
    let _ = std::fs::remove_dir_all(cl_plugins.join(plugin_pkg::verify::STAGING_DIR));
    for id_dir in std::fs::read_dir(cl_plugins).into_iter().flatten().flatten() {
        let id = id_dir.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') || id == plugin_pkg::verify::STAGING_DIR || !id_dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let approved = store
            .entries_for(&id)
            .find(|e| e.state == plugin_pkg::State::Allowed && e.platform == platform)
            .map(|e| e.version.clone())
            .filter(|v| id_dir.path().join(v.to_string()).is_dir());
        for e in std::fs::read_dir(id_dir.path()).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let set_aside = name.starts_with('.') && (name.ends_with(".replaced") || name.ends_with(".partial"));
            let older = approved.as_ref().is_some_and(|a| name.parse::<plugin_pkg::Version>().is_ok_and(|v| v.cmp_precedence(a).is_lt()));
            if (set_aside || older) && e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

// ---- drawing and input --------------------------------------------------------------------------

/// A card is open: it owns the cursor, and extensions get no `redraw` (§7.3 "No cover").
pub fn covers() -> bool {
    OFFERS.0.try_borrow().is_ok_and(|o| o.as_ref().is_some_and(|o| o.open.is_some()))
}

/// The cursor is the card's while one is open.
pub fn wants_cursor() -> bool {
    covers()
}

/// A string's width in the console font.
pub(crate) fn measure(s: &str) -> i32 {
    engine().console_string_size(s).0
}

/// A card's line height.
pub(crate) fn line_height() -> i32 {
    engine().console_string_size("Ay").1.max(12) + 4
}

fn layout_of(c: &Card) -> card::Layout {
    let s = crate::hud::screen();
    card::layout(c, s.width, s.height, line_height(), &measure)
}

/// `HUD_Redraw`, last: the open card over everything, and the downloads' progress.
pub fn redraw() {
    if OFFERS.0.borrow().is_none() {
        return;
    }
    draw_progress();
    let Some((c, armed, remaining, shift)) =
        with(|o| o.open.as_ref().map(|op| (op.card.clone(), op.arming.armed(), op.arming.remaining_secs(), o.shift)))
    else {
        return;
    };
    let l = layout_of(&c);
    draw_card(&l, &c.buttons, armed, remaining, shift);
    draw_cursor();
}

/// Paints a laid-out card: the panel, its lines and its buttons (an approving one greyed with its
/// countdown until `armed`). The plugins panel (`crate::plugins::menu`) paints with it too.
pub(crate) fn draw_card<A: card::Act>(l: &card::Layout, buttons: &[card::Button<A>], armed: bool, remaining: u64, shift: bool) {
    let e = engine();
    let p = l.panel;
    e.fill_blend(p.x - 2, p.y - 2, p.w + 4, p.h + 4, [200, 160, 60], 255);
    e.fill_blend(p.x, p.y, p.w, p.h, [16, 18, 24], 245);
    for (x, y, text, tone) in &l.lines {
        let rgb = match tone {
            card::Tone::Title => [1.0, 0.85, 0.4],
            card::Tone::Normal => [0.9, 0.9, 0.9],
            card::Tone::Dim => [0.6, 0.6, 0.65],
            card::Tone::Warn => [1.0, 0.4, 0.35],
        };
        e.draw_console_string(*x, *y, rgb, text);
    }
    let (cx, cy) = crate::input::cursor();
    for (b, r) in buttons.iter().zip(&l.buttons) {
        let (label, action) = b.current(shift);
        let enabled = !action.approves() || armed;
        let hover = r.contains(cx, cy) && enabled;
        let bg = match (action.approves(), enabled, hover) {
            (_, false, _) => [40, 40, 44],
            (true, true, true) => [70, 120, 70],
            (true, true, false) => [50, 90, 50],
            (false, _, true) => [80, 80, 90],
            (false, _, false) => [55, 55, 62],
        };
        e.fill_blend(r.x, r.y, r.w, r.h, bg, 255);
        let text = if enabled { label.to_owned() } else { format!("{label} ({remaining})") };
        let text = if b.wide { card::fit_text(&text, r.w - 2 * card::PAD, &measure) } else { text };
        let rgb = if enabled { [1.0, 1.0, 1.0] } else { [0.5, 0.5, 0.5] };
        let ty = r.y + (r.h - line_height()) / 2 + 2;
        e.draw_console_string(r.x + card::PAD, ty, rgb, &text);
    }
}

/// The virtual cursor, over a card.
pub(crate) fn draw_cursor() {
    let e = engine();
    let (cx, cy) = crate::input::cursor();
    e.fill_blend(cx - 1, cy - 6, 3, 13, [255, 255, 255], 255);
    e.fill_blend(cx - 6, cy - 1, 13, 3, [255, 255, 255], 255);
}

/// A line per download at the top of the screen; no cursor, the game goes on.
fn draw_progress() {
    let rows: Vec<String> = with(|o| {
        o.conn
            .iter()
            .flat_map(|c| &c.slots)
            .filter_map(|s| match &s.stage {
                Stage::Installing { progress, info, .. } => {
                    let done = progress.bytes.load(Ordering::Relaxed);
                    let pct = if s.offer.size == 0 { 100 } else { done * 100 / u64::from(s.offer.size) };
                    Some(format!(
                        "Downloading {} {}: {pct}% of {} (cl_plugins_cancel stops it)",
                        info.checked.manifest.name,
                        s.offer.version,
                        card::size_text(u64::from(s.offer.size))
                    ))
                }
                _ => None,
            })
            .collect()
    });
    if rows.is_empty() {
        return;
    }
    let e = engine();
    let lh = line_height();
    let w = rows.iter().map(|r| measure(r)).max().unwrap_or(0) + 2 * card::PAD;
    let sw = crate::hud::screen().width;
    let (x, y) = ((sw - w) / 2, 8);
    e.fill_blend(x, y, w, lh * rows.len() as i32 + 8, [16, 18, 24], 200);
    for (i, r) in rows.iter().enumerate() {
        e.draw_console_string(x + card::PAD, y + 4 + lh * i as i32, [0.9, 0.9, 0.9], r);
    }
}

/// Mouse buttons while a card is open (`IN_MouseEvent`): the left button's press and release.
pub fn mouse(state: i32, old: i32, x: i32, y: i32) {
    let (pressed, released) = (state & !old, old & !state);
    let action = with(|o| {
        let shift = o.shift;
        let op = o.open.as_mut()?;
        let s = crate::hud::screen();
        let l = card::layout(&op.card, s.width, s.height, line_height(), &measure);
        let at = card::button_at(&l, x, y);
        if pressed & 1 != 0 {
            op.clicks.press(at);
        }
        if released & 1 != 0 {
            let armed = op.arming.armed();
            return op.clicks.release(&op.card, at, armed, shift).map(|a| (a, op.from.clone(), op.card.clone()));
        }
        None
    });
    if let Some((a, from, c)) = action {
        with(|o| o.open = None);
        act(&from, &c, a);
    }
}

/// GoldSrc `keydefs.h`.
const K_ENTER: i32 = 13;
const K_ESCAPE: i32 = 27;
const K_SHIFT: i32 = 134;
const K_KP_ENTER: i32 = 169;

/// `HUD_Key_Event`, first: Shift is tracked for "Never"; while a card is open, Escape and Enter
/// choose its default ("Not now", never an approval) and are swallowed. No key presses an
/// approving button (§7.3).
pub fn key_event(down: bool, key: i32) -> bool {
    if key == K_SHIFT {
        with(|o| o.shift = down);
        return false;
    }
    if !covers() || !down || !matches!(key, K_ESCAPE | K_ENTER | K_KP_ENTER) {
        return false;
    }
    let Some(op) = with(|o| o.open.take()) else { return false };
    let a = op.card.default_action();
    act(&op.from, &op.card, a);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let cl = dir.path();
        let p = plugin_pkg::Platform::LinuxX86_64;
        for d in ["staging", "web/0.1.0", "web/0.2.0", "web/0.3.0", "web/.0.2.0.replaced", "web/.0.4.0.partial", "other/1.0.0"] {
            std::fs::create_dir_all(cl.join(d)).unwrap();
        }
        let mut store = plugin_pkg::Store::new();
        store.allow(plugin_pkg::Entry {
            id: "web".into(),
            version: "0.2.0".parse().unwrap(),
            state: plugin_pkg::State::Allowed,
            scope: Scope::Always,
            servers: vec![],
            source: "manual".into(),
            publisher_key: None,
            trust: plugin_pkg::Trust::Unsigned,
            approved_at: "2026-10-17T10:42:00Z".into(),
            manifest_sha256: None,
            platform: p,
            files: Default::default(),
        });
        clean_at_start(cl, &store, p);
        let left: Vec<bool> =
            ["staging", "web/0.1.0", "web/0.2.0", "web/0.3.0", "web/.0.2.0.replaced", "web/.0.4.0.partial", "other/1.0.0"]
                .iter()
                .map(|d| cl.join(d).exists())
                .collect();
        assert_eq!(left, [false, false, true, true, false, false, true], "older versions and leftovers go; a newer one waits for approval");
    }

    #[test]
    fn other_versions_go_but_the_loaded_one() {
        let dir = tempfile::tempdir().unwrap();
        let id = dir.path().join("web");
        for v in ["0.1.0", "0.2.0", "0.3.0", ".x"] {
            std::fs::create_dir_all(id.join(v)).unwrap();
        }
        remove_other_versions(&id, &id.join("0.3.0"), &[id.join("0.1.0")]);
        assert!(id.join("0.1.0").exists() && !id.join("0.2.0").exists() && id.join("0.3.0").exists() && id.join(".x").exists());
    }
}
