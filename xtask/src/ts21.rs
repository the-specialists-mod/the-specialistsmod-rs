//! `cargo xtask ts21 [roots...]`: TS 2.1 mode (`ts_gameplay 21`) on dedicated servers (issue
//! #103, docs/validation.md §2.y). On each root a dedicated server starts on ts_dojo with four
//! quota bots, its server config choosing ruleset 21, and xtask drives it by rcon through:
//!
//! 1. **The server config.** The Steam engines run it once the first map has loaded, so that map
//!    reloads once under 2.1; FWGS runs it before the first map too, which then starts under 2.1.
//!    Every player moves by `gp=21` and the bots roam.
//! 2. **rcon mid-map.** `ts_gameplay 30` reloads ts_dojo under 3.0, `ts_gameplay 21` back: one
//!    reload each, no loop.
//! 3. **LMS** (`lastmanstanding 1`, `respawnnumber 0`, DM): all but one player killed, the last
//!    one gets 10 frags.
//! 4. **The One** (`mp_theonemode 1`, ts_dojo): one player on `mp_theone`'s team, the others on
//!    `mp_theothers`'; killed, another One is picked.
//! 5. **Model teamplay** (`mp_teamplay 1`): every player on an `mp_teamlist` team; a killed player
//!    respawns.
//! 6. **Round team deathmatch** (cs_krustykrab): Specialists and Mercenaries; one respawn a round
//!    with `respawnnumber 2` (2.1 counts lives); three Specialist wins pay the team cash, then the
//!    sides swap.
//! 7. **Objective rounds** (tm_avana: a `ts_objective_manager`, two `ts_hack`s, `ts_wingiver`s,
//!    `ts_mapglobals` `playerrespawn 2`): the manager's reset, one respawn a round (two lives),
//!    the Specialists' death firing `targetonspecdead` (a Mercenary win giver), and the round time
//!    running out firing `targetontimeleft` (a draw giver).
//! 8. **A large map** (MecklenburgV_Rain_Final): its 3.0-only entities (`ts_wind`, `ts_fog`,
//!    `ts_emitter`, `ts_particle`, `ts_groundammo`) refused, and the bots playing.
//!
//! The bots stand still from 3 on (`ts_bot_stop 1`), so only xtask's `ts_hurt`s end lives. The
//! checks read `ts_status` replies, and the console lines the server prints with `developer` on:
//! the HUD texts (`[ts] hud:`), 2.1's rounds (`[ts] round (2.1):`) and the round manager
//! (`[ts] objective:`).
//!
//! After the servers, the Linux Steam roots also get the clients ([`Ctx::ts21_clients`]): the
//! port's client (FWGS, `fwgs` root) must be told ruleset 21 (`SrvSett` 4), and the original TS
//! 3.0 client (`~/games/xash_win_orig`, `TS_ORIG_CLIENT`) must be refused with 2.1's text.

use crate::a2s;
use crate::roots::{EXEC_SERVER_CFG, Engine, Os, Root};
use crate::{Ctx, say};
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const PASSWORD: &str = "xtask103";
/// The one-line config [`Server::changelevel`] writes.
const MAP_CFG: &str = "xtask_ts21_map.cfg";
const BOTS: usize = 4;

/// One player line of `ts_status`.
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub idx: u32,
    pub name: String,
    /// `alive`, `dead` or `observer`.
    pub state: String,
    pub frags: f32,
    pub cash: i32,
    pub team: String,
    pub gp: String,
    pub org: [f32; 3],
    /// `award`: 1 while a dead player has no respawn this round (`respawn_at < 0`).
    pub award: i32,
}

impl Player {
    fn alive(&self) -> bool {
        self.state == "alive"
    }

    /// Dead with no respawn left this round.
    fn out(&self) -> bool {
        !self.alive() && self.award == 1
    }
}

/// A `ts_status` reply.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// The rules' `Mode` (`Dm`, `TeamDm`, `RoundTdm`, `Objective`).
    pub mode: String,
    pub teamplay: bool,
    pub lms: bool,
    pub the_one: bool,
    pub time: f32,
    pub players: Vec<Player>,
}

impl Status {
    fn alive(&self) -> impl Iterator<Item = &Player> {
        self.players.iter().filter(|p| p.alive())
    }

    fn on(&self, team: &str) -> impl Iterator<Item = &Player> {
        self.players.iter().filter(move |p| p.team.eq_ignore_ascii_case(team))
    }

    fn player(&self, idx: u32) -> Option<&Player> {
        self.players.iter().find(|p| p.idx == idx)
    }
}

fn field<'a>(l: &'a str, key: &str) -> Option<&'a str> {
    let rest = &l[l.find(key)? + key.len()..];
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// The last `ts_status` block in `text`.
pub fn parse_status(text: &str) -> Option<Status> {
    let start = text.rfind("[ts] status: ")?;
    let mut lines = text[start..].lines();
    let head = lines.next()?;
    let mut s = Status {
        mode: field(head, "status: ")?.to_owned(),
        teamplay: field(head, "teamplay=") == Some("true"),
        lms: field(head, "lms=") == Some("true"),
        the_one: field(head, "theone=") == Some("true"),
        time: field(head, "time=")?.parse().ok()?,
        players: Vec::new(),
    };
    for l in lines {
        let Some(rest) = l.trim_start().strip_prefix("[ts]  #") else { continue };
        let Some((idx, rest)) = rest.split_once(' ') else { continue };
        let Some(hp) = rest.find(" hp=") else { continue };
        let Some((name, state)) = rest[..hp].trim_end().rsplit_once(' ') else { continue };
        let team = rest.split_once("team='").and_then(|(_, t)| t.split_once("' held=")).map_or("", |(t, _)| t);
        let org = rest.split_once("org=(").and_then(|(_, o)| o.split_once(')')).map_or("", |(o, _)| o);
        let o: Vec<f32> = org.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        s.players.push(Player {
            idx: idx.parse().unwrap_or(0),
            name: name.trim().to_owned(),
            state: state.to_owned(),
            frags: field(rest, "frags=").and_then(|v| v.parse().ok()).unwrap_or(0.0),
            cash: field(rest, "cash=").and_then(|v| v.parse().ok()).unwrap_or(0),
            team: team.to_owned(),
            gp: field(rest, "gp=").unwrap_or("").to_owned(),
            org: [o.first().copied().unwrap_or(0.0), o.get(1).copied().unwrap_or(0.0), o.get(2).copied().unwrap_or(0.0)],
            award: field(rest, "award=").and_then(|v| v.parse().ok()).unwrap_or(0),
        });
    }
    Some(s)
}

/// A dedicated server under test, driven by rcon. Everything sent and answered goes to
/// `transcript`, which the checks read with the console log.
struct Server<'a> {
    root: &'a Root,
    port: u16,
    child: Child,
    transcript: String,
    start: Instant,
}

impl Server<'_> {
    fn rcon(&mut self, cmd: &str) -> String {
        let goldsrc = self.root.engine != Engine::Fwgs;
        let mut text = None;
        for _ in 0..5 {
            match a2s::rcon("127.0.0.1", self.port, goldsrc, PASSWORD, cmd) {
                Ok(t) => {
                    text = Some(t);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_secs(1)),
            }
        }
        let text = text.unwrap_or_else(|| "(no rcon reply)\n".into());
        self.transcript += &format!("[{:6.1}] > {cmd}\n{text}", self.start.elapsed().as_secs_f32());
        text
    }

    fn status(&mut self) -> Option<Status> {
        let r = self.rcon("ts_status");
        parse_status(&r)
    }

    /// Polls `ts_status` about once a second until `ok` holds, for at most `secs` (twice that
    /// under Wine).
    fn wait(&mut self, secs: u64, ok: impl Fn(&Status) -> bool) -> Option<Status> {
        let limit = Duration::from_secs(if self.root.os == Os::Windows { secs * 2 } else { secs });
        let t = Instant::now();
        let mut last = None;
        while t.elapsed() < limit {
            if let Some(s) = self.status() {
                if ok(&s) {
                    return Some(s);
                }
                last = Some(s);
            }
            std::thread::sleep(Duration::from_millis(800));
        }
        self.transcript += &format!("(timed out; last status {last:?})\n");
        None
    }

    /// `changelevel map` (or a reload the server starts itself), then waits until the new map's
    /// clock (`ts_status` time) has started again and `ok` holds.
    fn wait_new_map(&mut self, before: f32, secs: u64, ok: impl Fn(&Status) -> bool) -> Option<Status> {
        self.wait(secs, |s| s.time < before && ok(s))
    }

    /// Kills player `idx` with `ts_hurt`, again each second while its spawn protection lasts.
    fn hurt(&mut self, idx: u32) {
        for _ in 0..8 {
            let r = self.rcon(&format!("ts_hurt {idx} 1000"));
            let health = r.split_once("health=").and_then(|(_, h)| h.split_whitespace().next()?.parse::<f32>().ok());
            if health.is_none_or(|h| h <= 0.0) {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// `changelevel map` through `exec` of a one-line config: run straight from rcon, the map
    /// load would print into the rcon reply, and some engines (ReHLDS's Windows release) then
    /// leave it out of the console log; `exec` queues the command for the next frame.
    fn changelevel(&mut self, map: &str) {
        fs::write(self.root.path.join("ts").join(MAP_CFG), format!("changelevel {map}\n")).unwrap();
        self.rcon(&format!("exec {MAP_CFG}"));
    }

    /// Kills `team`'s living players, and each one again once it has respawned, until every
    /// member is out for the round (the round then ends). False if that takes over 40 s.
    fn kill_team(&mut self, team: &str) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(40) {
            let Some(st) = self.status() else { continue };
            let members: Vec<&Player> = st.on(team).collect();
            if !members.is_empty() && members.iter().all(|p| p.out()) {
                return true;
            }
            for p in members.iter().filter(|p| p.alive()).map(|p| p.idx).collect::<Vec<_>>() {
                self.hurt(p);
            }
            sleep(1);
        }
        false
    }

    fn time(&mut self) -> f32 {
        self.status().map_or(f32::MAX, |s| s.time)
    }
}

fn sleep(secs: u64) {
    std::thread::sleep(Duration::from_secs(secs));
}

/// The server config: 2.1 for the first map (and debug output, cheats for `ts_hurt`, rcon), then
/// `servercfgfile` names a second config for every later map, which leaves `ts_gameplay` alone so
/// the rcon changes stick. HL25 rate-limits connectionless packets per address, which the rcon
/// polling would hit.
fn configs() -> [(&'static str, String); 2] {
    [
        (
            "xtask_ts21.cfg",
            format!(
                "// Written by cargo xtask ts21.\n{EXEC_SERVER_CFG}ts_gameplay 21\ndeveloper 1\nsv_cheats 1\nrcon_password {PASSWORD}\nsv_max_queries_sec 1000\nsv_max_queries_sec_global 10000\nmp_timelimit 0\nmp_fraglimit 0\nts_bot_quota {BOTS}\nservercfgfile xtask_ts21b.cfg\n"
            ),
        ),
        ("xtask_ts21b.cfg", format!("// Written by cargo xtask ts21.\nts_bot_quota {BOTS}\n")),
    ]
}

impl Ctx {
    pub fn ts21_all(&self, roots: &[Root], with_clients: bool) -> bool {
        let servers = self.parallel(roots, |r, i| self.ts21(r, 27615 + 10 * i as u16));
        let clients = !with_clients || self.ts21_clients(roots);
        say!("== ts21: dedicated servers {}, clients {}", crate::pass(servers), crate::pass(clients));
        servers && clients
    }

    fn ts21(&self, root: &Root, port: u16) -> bool {
        let gamedir = root.path.join("ts");
        for (name, text) in configs() {
            fs::write(gamedir.join(name), text).unwrap();
        }
        say!("== {}: TS 2.1 mode on a dedicated server, port {port}", root.name);
        let log = root.log_path();
        let _ = fs::remove_file(&log);
        let mut cmd = root.server_command_with("ts_dojo", port, "xtask_ts21.cfg", BOTS as u32);
        let out = fs::File::create(self.repo.join("target").join(format!("server-{}.log", root.name))).unwrap();
        cmd.stdout(out.try_clone().unwrap()).stderr(out);
        let child = cmd.spawn().expect("start server");
        let mut s = Server { root, port, child, transcript: String::new(), start: Instant::now() };
        let mut c: Vec<(String, bool)> = Vec::new();
        let up = {
            let limit = Duration::from_secs(if root.os == Os::Windows { 120 } else { 60 });
            let t = Instant::now();
            let mut up = false;
            while t.elapsed() < limit && !matches!(s.child.try_wait(), Ok(Some(_))) {
                std::thread::sleep(Duration::from_millis(500));
                if a2s::info("127.0.0.1", port).is_ok() {
                    up = true;
                    break;
                }
            }
            up
        };
        c.push(("server answered".into(), up));
        if up {
            run_scenes(&mut s, &mut c);
        }
        let _ = s.child.kill();
        let _ = s.child.wait();
        root.cleanup();
        for name in configs().map(|(n, _)| n).into_iter().chain([MAP_CFG]) {
            let _ = fs::remove_file(gamedir.join(name));
        }
        let transcript_path = self.repo.join("target").join(format!("ts21-{}.txt", root.name));
        fs::write(&transcript_path, &s.transcript).unwrap();
        let log = root.read_log(&self.repo);
        for l in log.lines().filter(|l| l.contains("[ts] gameplay") || l.contains("[ts] game rules") || l.contains("[ts] round (2.1)")) {
            say!("   | {}", l.trim_end());
        }
        c.extend(log_checks(&log, &s.transcript, root.engine));
        for (what, ok) in &c {
            say!("   {} {what}", crate::pass(*ok));
        }
        say!("   rcon transcript: {}", transcript_path.display());
        let ok = c.iter().all(|(_, ok)| *ok);
        say!("   {}", crate::pass(ok));
        ok
    }
}

/// The scenes, in order (module docs). Each pushes its checks.
fn run_scenes(s: &mut Server, c: &mut Vec<(String, bool)>) {
    // 1. The server config's 2.1, bots roaming.
    let first = s.wait(90, |st| st.players.len() >= BOTS && st.players.iter().all(|p| p.gp == "21"));
    c.push((format!("{BOTS} bots join the first map, every player at gp=21"), first.is_some()));
    // 2.1's bots respawn by pressing fire (they start as observers).
    s.wait(30, |st| st.alive().count() >= BOTS);
    let a = s.status();
    sleep(6);
    let b = s.status();
    let (moved, measured) = match (&a, &b) {
        (Some(a), Some(b)) => {
            let both: Vec<(&Player, &Player)> =
                a.alive().filter_map(|p| b.player(p.idx).filter(|q| q.alive() && q.name == p.name).map(|q| (p, q))).collect();
            (both.iter().filter(|(p, q)| crate::dist(p.org, q.org) > 64.0).count(), both.len())
        }
        _ => (0, 0),
    };
    c.push((
        format!("bots roam under 2.1: {moved} of {measured} living bots moved over 64 units in 6 s"),
        measured >= 2 && moved + 1 >= measured,
    ));

    // 2. rcon mid-map: 3.0 and back, one reload each. 60 s: a Wine root's reload with four bots
    // rejoining took over 40 s while all seven roots ran at once.
    let t = s.time();
    s.rcon("ts_gameplay 30");
    let to30 = s.wait_new_map(t, 60, |st| st.players.len() >= BOTS && st.players.iter().all(|p| p.gp == "30"));
    c.push(("rcon ts_gameplay 30 reloads ts_dojo under 3.0 (every player gp=30)".into(), to30.is_some()));
    let t = s.time();
    s.rcon("ts_gameplay 21");
    let to21 = s.wait_new_map(t, 60, |st| st.players.len() >= BOTS && st.players.iter().all(|p| p.gp == "21"));
    c.push(("rcon ts_gameplay 21 reloads ts_dojo under 2.1 (every player gp=21)".into(), to21.is_some()));
    sleep(4);
    let settled = s.time();
    sleep(3);
    c.push(("no further reload".into(), s.time() > settled));

    // 3. LMS.
    for cmd in ["ts_bot_stop 1", "respawnnumber 0", "lastmanstanding 1"] {
        s.rcon(cmd);
    }
    let lms = s.wait(30, |st| st.lms && st.alive().count() >= BOTS);
    let mut lms_ok = false;
    if let Some(st) = lms {
        // The round has (re)started and everyone is up; all but the first player go down.
        let mut alive: Vec<&Player> = st.alive().collect();
        alive.sort_by_key(|p| p.idx);
        let (last, before) = (alive[0].idx, alive[0].frags);
        for p in &alive[1..] {
            s.hurt(p.idx);
        }
        lms_ok = s.wait(15, |st| st.player(last).is_some_and(|p| p.frags >= before + 10.0)).is_some();
    }
    c.push(("LMS: the last player standing gets 10 frags".into(), lms_ok));
    for cmd in ["lastmanstanding 0", "respawnnumber 1"] {
        s.rcon(cmd);
    }

    // 4. The One. Its teams have to be in `mp_teamlist`: 2.1's `SetDefaultPlayerTeam`
    // (FUN_10038920) puts a player whose team isn't listed on the smallest listed team, and the
    // content's server.cfg lists team1..team4.
    s.rcon("mp_teamlist \"gordon;seal\"");
    s.rcon("mp_theonemode 1");
    let t = s.time();
    s.changelevel("ts_dojo");
    let one_team = |st: &Status| st.on("gordon").count() == 1 && st.on("seal").count() + 1 == st.players.len();
    let one = s.wait_new_map(t, 60, |st| st.the_one && st.players.len() >= BOTS && one_team(st));
    c.push(("The One: one player on mp_theone's team (gordon), the others on mp_theothers' (seal)".into(), one.is_some()));
    let mut again = false;
    if let Some(the_one) = one.as_ref().and_then(|st| st.on("gordon").next().map(|p| p.idx)) {
        s.hurt(the_one);
        // The dead One leaves its team at its respawn; another One is picked within 10 s.
        again = s.wait(30, |st| one_team(st) && st.on("gordon").next().is_some_and(|p| p.alive())).is_some();
    }
    c.push(("The One killed: a One is picked again".into(), again));
    s.rcon("mp_theonemode 0");

    // 5. Model teamplay, with TS's default team list.
    s.rcon("mp_teamlist \"hitman;gordon\"");
    s.rcon("mp_teamplay 1");
    let t = s.time();
    s.changelevel("ts_dojo");
    let listed = |st: &Status| st.players.iter().all(|p| p.team == "hitman" || p.team == "gordon");
    let tdm = s.wait_new_map(t, 60, |st| st.mode == "TeamDm" && !st.the_one && st.alive().count() >= BOTS && listed(st));
    c.push(("model teamplay: every player on an mp_teamlist team (hitman;gordon)".into(), tdm.is_some()));
    let mut respawned = false;
    if let Some(p) = tdm.as_ref().and_then(|st| st.alive().next().map(|p| p.idx)) {
        s.hurt(p);
        sleep(2);
        let dead = s.status().is_some_and(|st| st.player(p).is_some_and(|q| !q.alive()));
        respawned = dead && s.wait(30, |st| st.player(p).is_some_and(|q| q.alive())).is_some();
    }
    c.push(("model teamplay: a killed player respawns".into(), respawned));
    s.rcon("mp_teamplay 0");

    // 6. Round team deathmatch. `respawnnumber` counts lives (2.1's `Killed` counts the death
    // before `SetPlayerRespawnTime`): 2 is one respawn a round.
    s.rcon("respawnnumber 2");
    let t = s.time();
    s.changelevel("cs_krustykrab");
    let teams_up = |st: &Status| {
        st.alive().count() >= BOTS
            && st.alive().all(|p| p.team == "Specialists" || p.team == "Mercenary")
            && st.on("Mercenary").any(|p| p.alive())
    };
    let r1 = s.wait_new_map(t, 60, |st| st.mode == "RoundTdm" && teams_up(st));
    c.push(("round TDM on cs_krustykrab: every player on Specialists or Mercenary".into(), r1.is_some()));
    if let Some(st) = r1 {
        // One respawn a round: a Specialist killed comes back once, not twice.
        let spec = st.on("Specialists").find(|p| p.alive()).map(|p| p.idx);
        let mut once = false;
        if let Some(p) = spec {
            s.hurt(p);
            sleep(2);
            let back = s.wait(30, |st| st.player(p).is_some_and(|q| q.alive())).is_some();
            s.hurt(p);
            sleep(10);
            once = back && s.status().is_some_and(|st| st.player(p).is_some_and(|q| !q.alive()));
        }
        c.push(("round TDM: one respawn a round (respawnnumber 2: two lives)".into(), once));
        // Three Specialist wins, one life each from here: the Mercenaries all killed each round.
        s.rcon("respawnnumber 1");
        let mut wins = 0;
        for round in 0..3 {
            if round > 0 && s.wait(30, teams_up).is_none() {
                break;
            }
            if !s.kill_team("Mercenary") {
                break;
            }
            // The win ends the round and the next starts 3 s later with everyone up again.
            sleep(4);
            wins += 1;
        }
        c.push((format!("round TDM: {wins} of 3 rounds played to a Specialist win"), wins == 3));
        sleep(4);
    }

    // 7. Objective rounds.
    s.rcon("respawnnumber 1");
    s.rcon("roundtime 30");
    let t = s.time();
    s.changelevel("tm_avana");
    let r = s.wait_new_map(t, 60, |st| st.mode == "Objective" && teams_up(st));
    c.push(("objective rounds on tm_avana: the first round starts in mode 3 with both teams up".into(), r.is_some()));
    if let Some(st) = r {
        // `playerrespawn 2`, two lives: a Specialist killed comes back once, then sits the round
        // out.
        let mut once = false;
        if let Some(p) = st.on("Specialists").find(|p| p.alive()).map(|p| p.idx) {
            s.hurt(p);
            sleep(2);
            let back = s.wait(20, |st| st.player(p).is_some_and(|q| q.alive())).is_some();
            s.hurt(p);
            sleep(8);
            once = back && s.status().is_some_and(|st| st.player(p).is_some_and(|q| !q.alive()));
        }
        c.push(("objective rounds: one respawn a round (ts_mapglobals playerrespawn 2: two lives)".into(), once));
        // The rest of the Specialists (two lives each): the manager's `targetonspecdead`.
        c.push(("objective rounds: every Specialist out".into(), s.kill_team("Specialists")));
        sleep(4);
        // The next round runs out (`roundtime 30`): the manager's `targetontimeleft`.
        let next = s.wait(30, teams_up);
        c.push(("objective rounds: a new round after the Mercenary win".into(), next.is_some()));
        sleep(36);
    }
    s.rcon("roundtime 150");

    // 8. A large third-party map with 3.0-only entities. `developer 2` shows the refusals
    // (`at_aiconsole`).
    s.rcon("developer 2");
    let t = s.time();
    s.changelevel(MECK);
    let meck = s.wait_new_map(t, 90, |st| st.mode == "Dm" && st.alive().count() >= BOTS && st.players.iter().all(|p| p.gp == "21"));
    s.rcon("developer 1");
    c.push((format!("{MECK} under 2.1: every bot spawns and moves by gp=21"), meck.is_some()));
}

/// The large map scene 8 loads, and the 3.0-only classes on it 2.1 refuses (with their counts).
const MECK: &str = "MecklenburgV_Rain_Final";
const MECK_REFUSED: [(&str, usize); 5] = [("ts_wind", 1), ("ts_fog", 1), ("ts_emitter", 1), ("ts_particle", 2), ("ts_groundammo", 2)];

/// The checks on the console log (`log`) and the rcon replies (`rcon`).
pub fn log_checks(log: &str, rcon: &str, engine: Engine) -> Vec<(String, bool)> {
    let both = format!("{log}\n{rcon}");
    let has = |t: &str| both.contains(t);
    let count = |t: &str| log.matches(t).count();
    let first_map = log.lines().find_map(|l| l.split_once("[ts] gameplay: ruleset ").map(|(_, r)| r.trim_end().to_owned()));
    let first_ok = if engine == Engine::Fwgs {
        // FWGS ran the config before the first map.
        first_map.as_deref() == Some("21 for this map")
    } else {
        first_map.as_deref() == Some("30 for this map")
    };
    // The Steam engines: one reload for the server config; everyone: one for each rcon change.
    let config_reloads = if engine == Engine::Fwgs { 0 } else { 1 };
    let reloads21 = count("[ts] gameplay: ruleset 21: reloading ts_dojo");
    let reloads30 = count("[ts] gameplay: ruleset 30: reloading ts_dojo");
    let round = |t: &str| both.lines().any(|l| l.contains("[ts] round (2.1): ") && l.contains(t));
    let hud = |t: &str| both.lines().any(|l| l.contains("[ts] hud: ") && l.contains(t));
    let spec_cash = [4500, 6000, 7500].iter().all(|n| round(&format!("TeamWin(0, 1500, -1000): Specialists cash {n} score ")));
    vec![
        ("the Rust server library loaded".into(), log.contains("[ts] GameInit")),
        (
            format!("the first map {} under the server config's ruleset 21", if engine == Engine::Fwgs { "starts" } else { "reloads" }),
            first_ok,
        ),
        (
            format!("reloads: {reloads21} for 21 (expected {}), {reloads30} for 30 (expected 1); no loop", config_reloads + 1),
            reloads21 == config_reloads + 1 && reloads30 == 1,
        ),
        ("2.1's game rules: DeathMatch on ts_dojo".into(), has("[ts] game rules (2.1): DeathMatch")),
        ("3.0's game rules after ts_gameplay 30".into(), has("[ts] game rules: ")),
        ("LMS: \"is the Last Man Standing! 10 frags\"".into(), hud("is the Last Man Standing! 10 frags")),
        ("The One: 2.1's rules (\"The One (gordon;seal...\")".into(), has("[ts] game rules (2.1): The One (")),
        ("The One: \"is The One!\"".into(), hud("is The One!")),
        ("model teamplay: \"Team DeathMatch (hitman;gordon)\"".into(), has("[ts] game rules (2.1): Team DeathMatch (hitman;gordon)")),
        (
            "round TDM: \"Round Team DeathMatch (Specialists;Mercenary)\"".into(),
            has("[ts] game rules (2.1): Round Team DeathMatch (Specialists;Mercenary)"),
        ),
        ("round TDM: rounds start (\"Defeat the opposite team\")".into(), hud("Defeat the opposite team")),
        ("round TDM: \"Specialists Win\"".into(), hud("Specialists Win")),
        (
            "round TDM: the Specialists' team cash 4500, 6000, 7500 (cashforwin 1500), the Mercenaries' floor 3000".into(),
            spec_cash && round("Mercenary cash 3000 score 0"),
        ),
        ("round TDM: the sides swap after three rounds".into(), round("RestartRound: RoundTdm, ") && round(" s, sides swapped true")),
        (
            "objective rounds: RestartRound in mode 3 with 2 respawns a round".into(),
            round("RestartRound: Objective, 30 s, sides swapped false, respawns per round 2"),
        ),
        (
            "objective rounds: the manager's reset (\"Specialists must hack the computers!\")".into(),
            has("[ts] objective: the round's manager is #") && hud("Specialists must hack the computers!"),
        ),
        ("objective rounds: the Specialists' death fires \"mercwin\"".into(), has("the round's manager fires \"mercwin\" (SpecDead)")),
        (
            "objective rounds: the Mercenary win giver pays (TeamWin(1, 1500, -1000))".into(),
            round("TeamWin(1, 1500, -1000): Specialists cash 3000 score 0, Mercenary cash 4500 score 1"),
        ),
        (
            "objective rounds: the round time runs out and fires \"drawwin\"".into(),
            has("the round's manager fires \"drawwin\" (TimeLeft)") && round("TeamWin(2, "),
        ),
        (
            format!("{MECK}: the 3.0-only entities refused ({})", MECK_REFUSED.map(|(c, n)| format!("{n} {c}")).join(", ")),
            MECK_REFUSED.iter().all(|(class, n)| count(&format!("Can't init {class} (not in this ruleset)")) == *n),
        ),
        ("no panic or crash".into(), !(both.contains("PANIC") || both.contains("FATAL ERROR") || both.contains("Segmentation"))),
    ]
}

impl Ctx {
    /// The clients against 2.1 servers on the Linux Steam roots among `roots`: the port's FWGS
    /// client gets `SrvSett` 4 = 21 and plays; the original TS 3.0 client is refused.
    fn ts21_clients(&self, roots: &[Root]) -> bool {
        let fwgs = Root::all().into_iter().find(|r| r.name == "fwgs").unwrap();
        let orig = std::env::var_os("TS_ORIG_CLIENT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap()).join("games/xash_win_orig"));
        let servers: Vec<&Root> = roots.iter().filter(|r| r.engine != Engine::Fwgs && r.os == Os::Linux && r.exists()).collect();
        if servers.is_empty() {
            return true;
        }
        if !fwgs.exists() {
            say!("== ts21 clients: skipped (no fwgs root for the port's client)");
            return true;
        }
        self.install(&fwgs);
        let mut ok = true;
        for (i, r) in servers.into_iter().enumerate() {
            let port = 27715 + 10 * i as u16;
            ok &= self.ts21_client(r, &fwgs, port);
            if orig.join("xash3d.exe").exists() {
                ok &= self.ts21_orig_client(r, &orig, port + 1);
            } else {
                say!("== {}: the original TS client: skipped ({} missing)", r.name, orig.display());
            }
        }
        ok
    }

    /// Starts `root`'s 2.1 server for a client test; returns once it answers. Its stdin is a pipe
    /// (`xtask steam` feeds it server commands).
    pub(crate) fn ts21_client_server(&self, root: &Root, port: u16) -> Option<Child> {
        let gamedir = root.path.join("ts");
        for (name, text) in configs() {
            fs::write(gamedir.join(name), text).unwrap();
        }
        let _ = fs::remove_file(root.log_path());
        let mut cmd = root.server_command_with("ts_dojo", port, "xtask_ts21.cfg", 2);
        let out = fs::File::create(self.repo.join("target").join(format!("server-{}.log", root.name))).unwrap();
        cmd.stdin(std::process::Stdio::piped()).stdout(out.try_clone().unwrap()).stderr(out);
        let mut child = cmd.spawn().expect("start server");
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(500));
            if a2s::info("127.0.0.1", port).is_ok() {
                // The Steam engines reload the first map under 2.1 right away.
                sleep(8);
                return Some(child);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        None
    }

    pub(crate) fn ts21_client_stop(&self, root: &Root, mut child: Child) -> String {
        let _ = child.kill();
        let _ = child.wait();
        for (name, _) in configs() {
            let _ = fs::remove_file(root.path.join("ts").join(name));
        }
        root.read_log(&self.repo)
    }

    /// The port's client (FWGS, GoldSrc protocol) joins `root`'s 2.1 server: the client logs the
    /// ruleset from `SrvSett` 4 and the server lets it in.
    fn ts21_client(&self, root: &Root, fwgs: &Root, port: u16) -> bool {
        say!("== {}: the port's client (fwgs) joins a 2.1 server, port {port}", root.name);
        let Some(child) = self.ts21_client_server(root, port) else {
            say!("   FAIL: the server didn't answer");
            return false;
        };
        let w = "wait;".repeat(20);
        let cfg = format!(
            "connect 127.0.0.1:{port} gs\n{}echo XTASK: in game\n{}quit\n",
            format!("{w}\n").repeat(40),
            format!("{w}\n").repeat(3)
        );
        let gamedir = fwgs.path.join("ts");
        fs::write(gamedir.join("xtask_ts21_client.cfg"), cfg).unwrap();
        let log_path = self.repo.join("target").join(format!("ts21-client-{}.log", root.name));
        let log = fs::File::create(&log_path).unwrap();
        let mut client = Command::new(fwgs.path.join("xash3d"))
            .current_dir(&fwgs.path)
            .args(["-game", "ts", "-dev", "2", "-windowed", "-width", "800", "-height", "600", "+exec", "xtask_ts21_client.cfg"])
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .expect("start client");
        let t = Instant::now();
        while client.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = client.kill();
        let _ = client.wait();
        let _ = fs::remove_file(gamedir.join("xtask_ts21_client.cfg"));
        let server = self.ts21_client_stop(root, child);
        let text = String::from_utf8_lossy(&fs::read(&log_path).unwrap_or_default()).replace('\r', "");
        let checks = [
            ("the client connected", text.contains("client connected") || server.contains("entered the game")),
            (
                "the client: \"[ts] gameplay: the server runs ruleset 21\" (SrvSett 4)",
                text.contains("[ts] gameplay: the server runs ruleset 21"),
            ),
            ("the server didn't refuse it", !server.contains("[ts] gameplay: refused")),
            ("no panic", !text.contains("PANIC") && !server.contains("PANIC")),
        ];
        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        say!("   client log: {}", log_path.display());
        checks.iter().all(|(_, ok)| *ok)
    }

    /// The original TS 3.0 client (FWGS 0.21 win32 with TS's own `client.dll`, under Wine)
    /// connects with the GoldSrc protocol and must be refused with 2.1's text.
    fn ts21_orig_client(&self, root: &Root, orig: &std::path::Path, port: u16) -> bool {
        say!("== {}: the original TS 3.0 client is refused by a 2.1 server, port {port}", root.name);
        let Some(child) = self.ts21_client_server(root, port) else {
            say!("   FAIL: the server didn't answer");
            return false;
        };
        let w = "wait;".repeat(20);
        let cfg = format!("connect 127.0.0.1:{port} gs\n{}quit\n", format!("{w}\n").repeat(30));
        let cfg_path = orig.join("ts").join("xtask_ts21_orig.cfg");
        fs::write(&cfg_path, cfg).unwrap();
        let engine_log = orig.join("engine.log");
        let saved = fs::read(&engine_log).ok();
        let _ = fs::remove_file(&engine_log);
        let mut client = Command::new("wine")
            .current_dir(orig)
            .env("WINEPREFIX", crate::roots::wineprefix())
            .env("WINEDEBUG", "-all")
            .args([
                "xash3d.exe",
                "-game",
                "ts",
                "-dev",
                "2",
                "-log",
                "-windowed",
                "-width",
                "800",
                "-height",
                "600",
                "+exec",
                "xtask_ts21_orig.cfg",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the original client");
        let t = Instant::now();
        while client.try_wait().ok().flatten().is_none() && t.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = client.kill();
        let _ = client.wait();
        crate::roots::wine_cleanup();
        let _ = fs::remove_file(&cfg_path);
        let text = String::from_utf8_lossy(&fs::read(&engine_log).unwrap_or_default()).replace('\r', "");
        let dest = self.repo.join("target").join(format!("ts21-orig-client-{}.log", root.name));
        let _ = fs::write(&dest, &text);
        // Leave the original root's own log as it was.
        match saved {
            Some(b) => {
                let _ = fs::write(&engine_log, b);
            }
            None => {
                let _ = fs::remove_file(&engine_log);
            }
        }
        let server = self.ts21_client_stop(root, child);
        const REASON: &str = "Invalid Version: Running 2.1 mode. Upgrade to the rust port at tsmod.org";
        let shown = text.lines().find(|l| l.contains(REASON)).map(|l| l.trim().to_owned());
        say!("   the client shows: {}", shown.as_deref().unwrap_or("(nothing)"));
        let checks = [
            (
                "the server: \"[ts] gameplay: refused a client in ruleset 21 (Other)\"",
                server.contains("[ts] gameplay: refused a client in ruleset 21 (Other)"),
            ),
            ("the client was given 2.1's reason", shown.is_some()),
        ];
        for (what, ok) in &checks {
            say!("   {} {what}", crate::pass(*ok));
        }
        say!("   client log: {}", dest.display());
        checks.iter().all(|(_, ok)| *ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines() {
        let text = "junk\n[ts] status: RoundTdm teamplay=true lms=false theone=false time=12.5\n\
            [ts]  #1 Bot Number One   alive hp=100 ar=0 frags=2 deaths=1 cash=4500 team='Specialists' held=0x0/0s active=0x0 slow=1.00->1.00 award=11 spree=0 gp=21 org=(10 -20 36)\n\
            [ts]  #3 Mercer           observer hp=0 ar=0 frags=0 deaths=0 cash=3000 team='' held=0x0/0s active=0x0 slow=1.00->1.00 award=11 spree=0 gp=- org=(0 0 0)\n";
        let s = parse_status(text).unwrap();
        assert_eq!((s.mode.as_str(), s.teamplay, s.lms, s.the_one, s.time), ("RoundTdm", true, false, false, 12.5));
        assert_eq!(s.players.len(), 2);
        let p = &s.players[0];
        assert_eq!((p.idx, p.name.as_str(), p.state.as_str(), p.frags, p.cash), (1, "Bot Number One", "alive", 2.0, 4500));
        assert_eq!((p.team.as_str(), p.gp.as_str(), p.org), ("Specialists", "21", [10.0, -20.0, 36.0]));
        assert_eq!((s.players[1].state.as_str(), s.players[1].team.as_str(), s.players[1].gp.as_str()), ("observer", "", "-"));
        assert_eq!(s.alive().count(), 1);
        // The last block wins.
        let two = format!("{text}[ts] status: Dm teamplay=false lms=true theone=false time=1.0\n");
        assert_eq!(parse_status(&two).unwrap().players.len(), 0);
    }

    #[test]
    fn reload_counts() {
        let steam = "[ts] gameplay: ruleset 30 for this map\n[ts] gameplay: ruleset 21: reloading ts_dojo\n[ts] gameplay: ruleset 21 for this map\n\
            [ts] gameplay: ruleset 30: reloading ts_dojo\n[ts] gameplay: ruleset 30 for this map\n[ts] gameplay: ruleset 21: reloading ts_dojo\n";
        let get = |log: &str, e| {
            log_checks(log, "", e).into_iter().filter(|(w, _)| w.starts_with("reloads") || w.starts_with("the first map")).all(|(_, ok)| ok)
        };
        assert!(get(steam, Engine::ReHlds));
        assert!(!get(steam, Engine::Fwgs), "FWGS has no reload for the config");
        let fwgs = "[ts] gameplay: ruleset 21 for this map\n[ts] gameplay: ruleset 30: reloading ts_dojo\n[ts] gameplay: ruleset 21: reloading ts_dojo\n";
        assert!(get(fwgs, Engine::Fwgs));
        let looped = format!("{steam}[ts] gameplay: ruleset 21: reloading ts_dojo\n");
        assert!(!get(&looped, Engine::ReHlds), "a loop fails");
    }
}
