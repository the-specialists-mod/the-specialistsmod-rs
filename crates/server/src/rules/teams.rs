//! Teamplay bookkeeping (`CHalfLifeTeamplay`): default team, team change, TeamInfo/TeamNames,
//! team frag totals, The One's two teams. Team names per player index live in the rules state so
//! any code can read them without dispatching to the player.
//!
//! Team names are bytes, as TS's C strings are (#188): the `team`/`model` keys, `mp_teamlist`
//! and the map's `mapteams` go to `TeamInfo`, `TeamScore`, `TeamNames`, the SayText notices and
//! the log unconverted, and compare with `strcasecmp` (A-Z folded only, [`state::team_eq`]).

use super::msg;
use super::{netname, state};
use crate::damage::{DamageInfo, bits};
use crate::messages::Msg;
use crate::player::Player;
use crate::world::Cx;
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use shared::bformat;
use shared::protocol::rules::{TeamInfo, TeamNames, TeamScore};

fn idx(e: *mut edict_t) -> usize {
    if e.is_null() { 0 } else { engine().index_of(e).clamp(0, 32) as usize }
}

/// The team name of a player (`TeamID()`), empty outside teamplay.
pub fn team_name_of(e: *mut edict_t) -> Vec<u8> {
    let i = idx(e);
    state::with(|g| g.player_team.get(i).cloned().unwrap_or_default())
}

fn set_team_name(e: *mut edict_t, name: &[u8]) {
    let i = idx(e);
    state::with(|g| {
        if g.player_team.len() < 33 {
            g.player_team.resize(33, Vec::new());
        }
        g.player_team[i] = name.to_vec();
    });
}

/// `PlayerRelationship == GR_TEAMMATE`.
pub fn same_team(a: *mut edict_t, b: *mut edict_t) -> bool {
    if !state::teamplay() || a.is_null() || b.is_null() {
        return false;
    }
    let (ta, tb) = (team_name_of(a), team_name_of(b));
    !ta.is_empty() && state::team_eq(&ta, &tb)
}

/// `CTSGameRules::GetPlayerModel`: a random model from the team's list.
pub fn model_for(team: &[u8]) -> Vec<u8> {
    let models = state::with(|g| {
        let i = g.team_index(team);
        if i < 0 { Vec::new() } else { g.teams[i as usize].models.clone() }
    });
    if models.is_empty() {
        return Vec::new();
    }
    models[engine().random_long(0, models.len() as i32 - 1) as usize].clone()
}

/// Every player's team name and frags, in index order (`UTIL_PlayerByIndex` + `IsPlayer`, which
/// is always true for a player). Players who left count too, with the empty team and zero frags
/// `TSPlayerDisconnect` gave them.
fn player_teams() -> Vec<(*mut edict_t, Vec<u8>, f32)> {
    super::slot_players().into_iter().map(|e| (e, team_name_of(e), e.v().frags)).collect()
}

/// `m_szTeamName` is a 16-byte field filled with `strncpy(.., 16)`: longer names keep their
/// first 16 bytes, whatever they are (a cut through a UTF-8 sequence keeps the partial bytes, as
/// TS does). (TS leaves such a name unterminated, so its reads run on into the next field, the
/// animation extension at `+0xb58`; the port stops at 16 bytes.) A NUL can't reach it: names come
/// from C strings.
fn team_field(name: &[u8]) -> Vec<u8> {
    name[..name.len().min(16)].to_vec()
}

/// `TSPlayerDisconnect` clears `m_szTeamName`.
pub fn clear_team(p: &mut Player, e: *mut edict_t) {
    store_team(p, e, b"");
}

/// `CHalfLifeTeamplay::RecountTeams` (0xe5608): rebuilds the team list and team frags. With
/// `send` (`SetDefaultPlayerTeam`), every player on a valid team also goes to everyone as a
/// `TeamInfo`, and in CTB that player gets the `TeamScore` of every listed team.
pub fn recount(send: bool) {
    let players = player_teams();
    let names: Vec<(Vec<u8>, f32)> = players.iter().map(|(_, n, f)| (n.clone(), *f)).collect();
    state::with(|g| g.recount_teams(&names));
    if !send {
        return;
    }
    let ctb = state::with(|g| g.mode == state::Mode::TeamDm && g.is_ctb());
    let scores: Vec<(Vec<u8>, i32)> =
        state::with(|g| g.teams.iter().filter(|t| !t.name.is_empty()).map(|t| (t.name.clone(), t.score)).collect());
    let eng = engine();
    for (e, team, _) in players {
        if !state::with(|g| g.is_valid_team(&team)) {
            continue;
        }
        let idx = eng.index_of(e);
        msg::all(Msg::TeamInfo, |w| TeamInfo { idx: idx as u8, team: team.clone() }.write(w));
        if ctb {
            for (name, score) in &scores {
                let m = TeamScore { team: name.clone(), score: *score as i16, deaths: 0 };
                msg::one(Msg::TeamScore, e, |w| m.write(w));
            }
        }
    }
}

/// The `team` field of a player's `ScoreInfo`: `g_pGameRules->GetTeamIndex(m_szTeamName) + 1`
/// at send time (`CHalfLifeTeamplay::GetTeamIndex` 0xe5434), 0 outside teamplay or for a name
/// the last `RecountTeams` didn't list.
pub fn score_team(idx: i32) -> i16 {
    state::with(|g| {
        let name = g.player_team.get(idx.clamp(0, 32) as usize).cloned().unwrap_or_default();
        (g.tp_team_index(&name) + 1) as i16
    })
}

/// `TeamWithFewestPlayers` (0xe5510), counted from the players' current teams.
fn fewest() -> Vec<u8> {
    let names: Vec<Vec<u8>> = player_teams().into_iter().map(|(_, n, _)| n).collect();
    state::with(|g| g.team_with_fewest(&names))
}

/// Whether `SetDefaultPlayerTeam` keeps the team it read: a non-empty, valid name and no
/// `mp_defaultteam` (0xe4570..0xe45a0). Otherwise `team_names[0]` under `mp_defaultteam`, else
/// the smallest team.
fn default_team_kept(name: &[u8], valid: bool, defaultteam: bool) -> bool {
    !name.is_empty() && valid && !defaultteam
}

/// `CHalfLifeTeamplay::SetDefaultPlayerTeam` (0xe44ac), on every teamplay spawn and InitHUD:
/// reads the `team` key (The One mode: The One's team or the other one), recounts with
/// `TeamInfo`, then falls back to `team_names[0]` or the smallest team. It only stores the name
/// (`m_szTeamName`); the `model` and `team` keys are `ChangePlayerTeam`'s.
pub fn set_default_team(p: &mut Player, cx: &mut Cx) {
    let e = cx.edict;
    let name = crate::gameplay::active().server.default_team(p, cx);
    store_team(p, e, &name);
    recount(true);
    let defaultteam = super::cvars::on(c"mp_defaultteam");
    if !default_team_kept(&name, state::with(|g| g.is_valid_team(&name)), defaultteam) {
        let name = if defaultteam { state::with(|g| g.first_team_slot()) } else { fewest() };
        store_team(p, e, &name);
    }
}

/// Stores the team name on the player (`m_szTeamName`).
fn store_team(p: &mut Player, e: *mut edict_t, name: &[u8]) {
    let name = team_field(name);
    set_team_name(e, &name);
    p.ts.team_name = name;
}

/// The `model` key `ChangePlayerTeam` always sets (0xe4a0f..0xe4b28): with a team limit
/// (`+0x214`) `CTSGameRules::GetPlayerModel` (0x79a08), a random one of the team's models or `""`
/// when it has none (`pick(n)` gives `RANDOM_LONG(0, n - 1)`); without one the team name itself.
fn change_team_model(team_limit: bool, team: &[u8], models: &[Vec<u8>], pick: impl FnOnce(i32) -> i32) -> Vec<u8> {
    if !team_limit {
        return team.to_vec();
    }
    if models.is_empty() {
        return Vec::new();
    }
    models.get(pick(models.len() as i32) as usize).cloned().unwrap_or_default()
}

/// The team `ChangePlayerTeam` (0xe488c) settles on: in The One mode The One goes to the first
/// `TSGameRules` team and everyone else to the second, whatever was asked; otherwise an empty
/// name or one `IsValidTeam` refuses means the smallest team (`None`). Without a team limit any
/// name is valid. A ruleset that doesn't check the team
/// ([`TeamChangeRules::change_checks_team`](crate::gameplay::server::TeamChangeRules::change_checks_team))
/// takes any non-empty name.
fn change_team_choice(
    checked: bool,
    requested: &[u8],
    the_one: bool,
    is_the_one: bool,
    one: &[Vec<u8>; 2],
    valid: bool,
) -> Option<Vec<u8>> {
    if !checked {
        return (!requested.is_empty()).then(|| requested.to_vec());
    }
    if the_one {
        return Some(one[if is_the_one { 0 } else { 1 }].clone());
    }
    (!requested.is_empty() && valid).then(|| requested.to_vec())
}

/// `CHalfLifeTeamplay::ChangePlayerTeam` (0xe488c): `jointeam` and the teamplay `InitHUD`.
pub fn change_team(p: &mut Player, cx: &mut Cx, name: &[u8], kill: bool, gib: bool) {
    // TS doesn't check that the player is alive (0xe48e2..0xe4954): `TakeDamage(900)` with the
    // death message and penalty (`+0x20c`/`+0x210`) off for the call.
    if kill {
        let world = engine().entity_of_index(0);
        let b = if gib { bits::ALWAYSGIB } else { bits::NEVERGIB };
        state::with(|g| g.death_suppressed = true);
        // #326: TS calls the virtual `TakeDamage` (`ChangePlayerTeam` 0xe48e2).
        crate::ham::player_take_damage(p, cx, &DamageInfo::new(world, world, 900.0, b));
        state::with(|g| g.death_suppressed = false);
    }
    let (the_one, one, valid) = state::with(|g| (g.the_one, g.the_one_team_names(), g.is_valid_team(name)));
    let checked = crate::gameplay::active().server.team_changes().change_checks_team;
    let name = change_team_choice(checked, name, the_one, p.ts.is_the_one, &one, valid).unwrap_or_else(fewest);
    let e = cx.edict;
    store_team(p, e, &name);
    let name = p.ts.team_name.clone();
    let (limit, models) = state::with(|g| {
        let i = g.team_index(&name);
        (g.team_limit, if i < 0 { Vec::new() } else { g.teams[i as usize].models.clone() })
    });
    let eng = engine();
    eng.set_client_key(e, "model", &change_team_model(limit, &name, &models, |n| eng.random_long(0, n - 1)));
    // In the round modes the objective list again (`SendObjectiveList`).
    crate::entities::ts::objective::resend_list(e);
    eng.set_client_key(e, "team", &name);
    recount(false);
    let idx = cx.index();
    msg::all(Msg::TeamInfo, |w| TeamInfo { idx: idx as u8, team: name.clone() }.write(w));
    let s = super::lifecycle::score_info_of(p, cx.pev(), idx);
    msg::all(Msg::ScoreInfo, |w| s.write(w));
    crate::gameplay::active().server.change_team_tail(p, cx, kill);
    send_team_names(std::ptr::null_mut());
}

/// `TeamNames` to one player (or everyone with a null edict).
pub fn send_team_names(to: *mut edict_t) {
    let t = TeamNames { names: state::with(|g| g.team_names()) };
    if to.is_null() {
        msg::all(Msg::TeamNames, |w| t.write(w));
    } else {
        msg::one(Msg::TeamNames, to, |w| t.write(w));
    }
}

/// Teamplay part of `InitHUD` (0xe45d8): TeamNames, the assignment notice (`model` key equal to
/// the team, case-sensitive), `ChangePlayerTeam`, then the TeamInfo of every player on a valid
/// team (any team without a team limit).
pub fn init_hud(p: &mut Player, cx: &mut Cx) {
    let e = cx.edict;
    send_team_names(e);
    recount(false);
    let name = p.ts.team_name.clone();
    let model = engine().info_key(e, c"model").to_bytes().to_vec();
    let text = init_hud_notice(&model, &name);
    change_team(p, cx, &name, false, false);
    msg::say_one(e, cx.index(), &text);
    recount(false);
    let eng = engine();
    // `UTIL_PlayerByIndex`: players who left are listed too (with no team, valid without a limit).
    for i in 1..=eng.max_clients() {
        let o = eng.player(i);
        if !super::has_player(o) {
            continue;
        }
        let t = team_name_of(o);
        if state::with(|g| g.is_valid_team(&t)) {
            msg::one(Msg::TeamInfo, e, |w| TeamInfo { idx: i as u8, team: t.clone() }.write(w));
        }
    }
}

/// The teamplay `InitHUD`'s assignment line (0xe45d8): `* you are on team '%s'` when the `model`
/// key is the team name byte for byte (`strcmp`), else `* assigned to team %s`.
fn init_hud_notice(model: &[u8], team: &[u8]) -> Vec<u8> {
    if model == team { bformat!("* you are on team '{}'\n", team) } else { bformat!("* assigned to team {}\n", team) }
}

/// `ClientUserInfoChanged`'s (0xe4d18) `* %s has changed to team '%s'` to all.
fn team_change_notice(name: &[u8], team: &[u8]) -> Vec<u8> {
    bformat!("* {} has changed to team '{}'\n", name, team)
}

/// `TeamScore` of every team to all (after captures / wins).
pub fn send_team_scores() {
    let teams: Vec<(Vec<u8>, i32)> =
        state::with(|g| g.teams.iter().filter(|t| !t.name.is_empty()).map(|t| (t.name.clone(), t.score)).collect());
    for (team, score) in teams {
        let m = TeamScore { team, score: score as i16, deaths: 0 };
        msg::all(Msg::TeamScore, |w| m.write(w));
    }
}

/// The One's team switch in `SelectRandomTheOne` and `TheOneIsDead` (`CTSGameRules.c:2189`,
/// `:2270-2275`): `g_pGameRules->SetDefaultPlayerTeam(p)`, then
/// `g_pGameRules->ChangePlayerTeam(p, p->TeamID(), FALSE, FALSE)`, which in The One mode puts The
/// One on the first team and everyone else on the second, sets the `model` and `team` keys and
/// sends `TeamInfo`, `ScoreInfo` and `TeamNames` (no observer: The One mode).
pub fn the_one_switch(p: &mut Player, cx: &mut Cx) {
    if !state::teamplay() {
        return;
    }
    set_default_team(p, cx);
    let name = p.ts.team_name.clone();
    change_team(p, cx, &name, false, false);
}

/// `the_one_switch` for a player other than the one being dispatched.
pub fn the_one_switch_other(e: *mut edict_t) {
    crate::player::with_player(e, the_one_switch);
}

/// What `ClientUserInfoChanged` does first with a changed `model` key, before `mp_defaultteam`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelChange {
    /// The key becomes a team model (`GetPlayerModel`).
    TeamModel,
    /// `SetDefaultPlayerTeam` runs again.
    DefaultTeam,
    /// On to `mp_defaultteam` and the team change.
    TeamChange,
}

/// The head of `ClientUserInfoChanged`: the round modes always reset the model; then a
/// ruleset's team-limit reset (3.0 0xe4d18, `+0x214`) or its The One case (2.1 0x10038f50, vtable
/// +0xdc).
fn model_change(r: &crate::gameplay::server::TeamChangeRules, round: bool, limit: bool, the_one: bool) -> ModelChange {
    if round || (r.model_change_team_limit && limit) {
        ModelChange::TeamModel
    } else if r.model_change_the_one_default && the_one {
        ModelChange::DefaultTeam
    } else {
        ModelChange::TeamChange
    }
}

/// `CHalfLifeTeamplay::ClientUserInfoChanged` (0xe4d18), after the Multiplay part: a `model` key
/// that differs (case-insensitively) from the team name. In the round modes (unreachable in 3.0)
/// it is replaced by a random team model (`GetPlayerModel`), and so it is with a team limit or
/// The One's teams are set again, as the ruleset has it ([`model_change`]). Otherwise it is a
/// team change, or under `mp_defaultteam` both keys are put back.
pub fn userinfo_changed(p: &mut Player, cx: &mut Cx) {
    let e = cx.edict;
    let eng = engine();
    let model = eng.info_key(e, c"model").to_bytes().to_vec();
    let team = p.ts.team_name.clone();
    if state::team_eq(&model, &team) {
        return;
    }
    let (mode, limit, the_one) = state::with(|g| (g.mode, g.team_limit, g.the_one));
    let idx = cx.index();
    let round = matches!(mode, state::Mode::RoundTdm | state::Mode::Objective);
    match model_change(crate::gameplay::active().server.team_changes(), round, limit, the_one) {
        ModelChange::TeamModel => {
            eng.set_client_key(e, "model", &model_for(&team));
            return;
        }
        ModelChange::DefaultTeam => {
            set_default_team(p, cx);
            return;
        }
        ModelChange::TeamChange => {}
    }
    if super::cvars::on(c"mp_defaultteam") {
        eng.set_client_key(e, "model", &team);
        eng.set_client_key(e, "team", &team);
        msg::say_one(e, idx, "* Not allowed to change teams in this game!\n");
        return;
    }
    if !state::with(|g| g.is_valid_team(&model)) {
        // Unreachable with a team-limit reset: without a team limit every name is valid.
        eng.set_client_key(e, "model", &team);
        msg::say_one(e, idx, &bformat!("* Can't change team to '{}'\n", model));
        let list: Vec<u8> = state::with(|g| g.team_list.iter().flat_map(|t| bformat!("{};", t)).collect());
        msg::say_one(e, idx, &bformat!("* Server limits teams to '{}'\n", list));
        return;
    }
    msg::say_all(idx, &team_change_notice(&netname(e), &model));
    msg::log(&bformat!("\"{}<{}><{}><{}>\" joined team \"{}\"\n", netname(e), eng.user_id(e), eng.auth_id(e).to_bytes(), team, model));
    change_team(p, cx, &model, false, false);
    recount(true);
}

#[cfg(test)]
mod tests {
    use super::{
        ModelChange, change_team_choice, change_team_model, default_team_kept, init_hud_notice, model_change, team_change_notice,
        team_field,
    };
    use shared::protocol::hud_wire::Bytes;
    use shared::protocol::rules::{TeamInfo, TeamNames, TeamScore};

    #[test]
    fn team_names_keep_sixteen_bytes() {
        // strncpy(m_szTeamName, name, 16).
        assert_eq!(team_field(b"hitman"), b"hitman");
        assert_eq!(team_field(b"abcdefghijklmnopqrst"), b"abcdefghijklmnop");
    }

    /// #188: the 16-byte cut counts bytes and keeps them: a CP1252 0xE9 stays 0xE9, and a cut
    /// through a UTF-8 sequence keeps its first byte (TS's `strncpy`), never U+FFFD (`EF BF BD`).
    #[test]
    fn team_names_are_bytes() {
        assert_eq!(team_field(b"\xe9quipe"), b"\xe9quipe");
        // 15 ASCII bytes, then "é" in UTF-8 (C3 A9): the cut falls between C3 and A9.
        let cut = team_field("abcdefghijklmnoé".as_bytes());
        assert_eq!(cut, b"abcdefghijklmno\xc3");
        assert!(!cut.windows(3).any(|w| w == b"\xef\xbf\xbd"));
        // That name goes to TeamInfo as those 16 bytes, and the client reads them back.
        let mut w = Bytes::default();
        TeamInfo { idx: 3, team: cut.clone() }.write(&mut w);
        assert_eq!(w.buf, b"\x03abcdefghijklmno\xc3\0");
        let client = shared::protocol::hud::TeamInfo::decode(&w.buf).unwrap();
        assert_eq!(shared::text::latin1_bytes(&client.team), cut);
        // TeamScore and TeamNames carry the names byte for byte too.
        let mut w = Bytes::default();
        TeamScore { team: b"\xe9quipe".to_vec(), score: 2, deaths: 0 }.write(&mut w);
        assert_eq!(w.buf, b"\xe9quipe\0\x02\0\0\0");
        let mut w = Bytes::default();
        TeamNames { names: vec![b"\xe9quipe".to_vec(), cut.clone()] }.write(&mut w);
        assert_eq!(w.buf, b"\x02\xe9quipe\0abcdefghijklmno\xc3\0");
        let names = shared::protocol::hud::decode_team_names(&w.buf).unwrap();
        assert_eq!(names.iter().map(|n| shared::text::latin1_bytes(n)).collect::<Vec<_>>(), [b"\xe9quipe".to_vec(), cut]);
    }

    /// The SayText lines keep the names' bytes (`sprintf` in `InitHUD` 0xe45d8 and
    /// `ClientUserInfoChanged` 0xe4d18).
    #[test]
    fn team_lines_keep_bytes() {
        assert_eq!(init_hud_notice(b"\xe9quipe", b"\xe9quipe"), b"* you are on team '\xe9quipe'\n");
        // strcmp: a key that differs only in case (or in a non-ASCII byte) is "assigned".
        assert_eq!(init_hud_notice(b"\xc9QUIPE", b"\xe9quipe"), b"* assigned to team \xe9quipe\n");
        assert_eq!(init_hud_notice(b"\xe9QUIPE", b"\xe9quipe"), b"* assigned to team \xe9quipe\n");
        assert_eq!(team_change_notice(b"Ren\xe9", b"\xe9quipe"), b"* Ren\xe9 has changed to team '\xe9quipe'\n");
    }
    use crate::rules::state::GameRules;

    fn one() -> [Vec<u8>; 2] {
        [b"The ONE".to_vec(), b"gordon".to_vec()]
    }

    /// The rules after the Teamplay constructor for `list`.
    fn teamplay(list: &str) -> GameRules {
        let mut g = GameRules { teams: (0..4).map(|_| Default::default()).collect(), ..Default::default() };
        g.set_team_list(list.as_bytes());
        g.init_teamplay_list();
        g
    }

    #[test]
    fn change_team_picks_a_valid_team_or_the_smallest() {
        let g = teamplay("hitman;gordon");
        let choice = |name: &str| change_team_choice(true, name.as_bytes(), false, false, &one(), g.is_valid_team(name.as_bytes()));
        assert_eq!(choice("GORDON").as_deref(), Some(&b"GORDON"[..]));
        assert_eq!(choice("hitman").as_deref(), Some(&b"hitman"[..]));
        // `jointeam` out of range: GetIndexedTeamName gives "" and TS picks the smallest team.
        assert_eq!(choice(""), None);
        assert_eq!(choice("seal"), None);
    }

    #[test]
    fn change_team_without_a_team_limit_takes_any_name() {
        let g = teamplay("hitman");
        let choice = |name: &str| change_team_choice(true, name.as_bytes(), false, false, &one(), g.is_valid_team(name.as_bytes()));
        assert_eq!(choice("seal").as_deref(), Some(&b"seal"[..]));
        assert_eq!(choice(""), None, "an empty name still goes to the smallest team");
    }

    #[test]
    fn change_team_in_the_one_mode_ignores_the_request() {
        assert_eq!(change_team_choice(true, b"gordon", true, true, &one(), false).as_deref(), Some(&b"The ONE"[..]));
        assert_eq!(change_team_choice(true, b"hitman", true, false, &one(), true).as_deref(), Some(&b"gordon"[..]));
    }

    #[test]
    fn default_team_keeps_a_valid_team_key() {
        assert!(default_team_kept(b"gordon", true, false));
        // mp_defaultteam always replaces it (with team_names[0]).
        assert!(!default_team_kept(b"gordon", true, true));
        assert!(!default_team_kept(b"seal", false, false));
        assert!(!default_team_kept(b"", true, false));
    }

    #[test]
    fn change_team_model_follows_the_team_limit() {
        let models = vec![b"seal".to_vec(), b"merc".to_vec()];
        // No team limit: the team name is the model, whatever the list holds.
        assert_eq!(change_team_model(false, b"hitman", &models, |_| unreachable!()), b"hitman");
        // With one: GetPlayerModel picks from the team's models (RANDOM_LONG(0, n - 1)).
        assert_eq!(change_team_model(true, b"hitman", &models, |n| n - 1), b"merc");
        assert_eq!(change_team_model(true, b"hitman", &models, |_| 0), b"seal");
        // A team without models (or an unknown one) still sets the key, to "".
        assert_eq!(change_team_model(true, b"hitman", &[], |_| unreachable!()), b"");
    }

    // Audit round 2: B2
    use crate::gameplay::server::TeamChangeRules;

    /// 3.0's checks (`ChangePlayerTeam` 0xe488c, `ClientUserInfoChanged` 0xe4d18).
    const CHECKED: TeamChangeRules =
        TeamChangeRules { change_checks_team: true, model_change_team_limit: true, model_change_the_one_default: false };
    /// 2.1's (0x10038cb0, 0x10038f50).
    const UNCHECKED: TeamChangeRules =
        TeamChangeRules { change_checks_team: false, model_change_team_limit: false, model_change_the_one_default: true };

    /// #238: without the checks only an empty name goes to the smallest team; The One mode and
    /// a name `IsValidTeam` refuses keep the request (0x10038d32..0x10038d58).
    #[test]
    fn unchecked_change_takes_the_requested_team() {
        assert_eq!(change_team_choice(false, b"gordon", true, false, &one(), false).as_deref(), Some(&b"gordon"[..]));
        assert_eq!(change_team_choice(false, b"The ONE", true, false, &one(), true).as_deref(), Some(&b"The ONE"[..]));
        assert_eq!(change_team_choice(false, b"seal", false, false, &one(), false).as_deref(), Some(&b"seal"[..]));
        assert_eq!(change_team_choice(false, b"", false, false, &one(), true), None);
        assert_eq!(change_team_choice(false, b"", true, true, &one(), true), None);
    }

    /// #238: `ClientUserInfoChanged`'s first branch per ruleset.
    #[test]
    fn model_change_per_ruleset() {
        use ModelChange::*;
        // The round modes reset the model in both.
        assert_eq!(model_change(&CHECKED, true, true, false), TeamModel);
        assert_eq!(model_change(&UNCHECKED, true, true, false), TeamModel);
        // A team limit: 3.0 resets the model, 2.1 changes team.
        assert_eq!(model_change(&CHECKED, false, true, false), TeamModel);
        assert_eq!(model_change(&UNCHECKED, false, true, false), TeamChange);
        // The One mode: 2.1 sets The One's teams again, 3.0 has no such case.
        assert_eq!(model_change(&UNCHECKED, false, true, true), DefaultTeam);
        assert_eq!(model_change(&CHECKED, false, false, true), TeamChange);
        assert_eq!(model_change(&CHECKED, false, false, false), TeamChange);
    }
}
