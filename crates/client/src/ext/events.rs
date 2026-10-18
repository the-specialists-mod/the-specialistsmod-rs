//! What the extensions' `game_event` and `usercmd` callbacks get, worked out from the port's HUD
//! messages and commands (docs/implementation/client-ext-api.md §5). Pure, so it is tested
//! without an engine; [`super`] feeds it and calls the extensions.

use shared::protocol::Msg;
use shared::protocol::hud as p;
use std::ffi::c_char;
use ts_client_ext_api::{GameEvent, NAME_LEN, event};

/// The local values the HUD shows that the events report changes of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub health: i32,
    pub armor: i32,
    pub weapon: i32,
    pub team: i32,
}

/// Copies `bytes` into a fixed C string, cut to fit and NUL-terminated.
pub fn fill<const N: usize>(buf: &mut [c_char; N], bytes: &[u8]) {
    let n = bytes.len().min(N - 1);
    for (d, s) in buf.iter_mut().zip(&bytes[..n]) {
        *d = *s as c_char;
    }
    buf[n..].fill(0);
}

/// A [`GameEvent`].
pub fn make(kind: i32, a: i32, b: i32, c: i32, text: &[u8]) -> GameEvent {
    let mut e = GameEvent { kind, a, b, c, text: [0; NAME_LEN] };
    fill(&mut e.text, text);
    e
}

/// The events a change of the HUD's local values makes: health and armour together, the weapon
/// (with its name from `name`), the team.
pub fn changes(old: Snapshot, new: Snapshot, name: impl Fn(i32) -> String) -> Vec<GameEvent> {
    let mut out = Vec::new();
    if (old.health, old.armor) != (new.health, new.armor) {
        out.push(make(event::HEALTH, new.health, new.armor, 0, b""));
    }
    if old.weapon != new.weapon {
        out.push(make(event::WEAPON, new.weapon, 0, 0, name(new.weapon).as_bytes()));
    }
    if old.team != new.team {
        out.push(make(event::TEAM, new.team, 0, 0, b""));
    }
    out
}

/// The event a HUD message is itself, if any: `ResetHUD` a spawn, `DeathMsg` a death notice,
/// `RoundTime` the round timer, `InitHUD` a map (`map` its name). `local` is the local player's
/// entity index.
pub fn from_message(m: Msg, buf: &[u8], local: i32, map: &str) -> Option<GameEvent> {
    match m {
        Msg::ResetHUD => Some(make(event::SPAWN, 0, 0, 0, b"")),
        Msg::InitHUD => Some(make(event::MAP, 0, 0, 0, map.as_bytes())),
        Msg::DeathMsg => {
            let d = p::DeathMsg::decode(buf)?;
            let mine = local > 0 && d.victim as i32 == local;
            Some(make(event::DEATH, d.killer as i32, d.victim as i32, mine as i32, d.weapon.as_bytes()))
        }
        Msg::RoundTime => {
            let r = p::RoundTime::decode(buf)?;
            Some(make(event::ROUND, r.secs as i32, r.mode as i32, 0, b""))
        }
        _ => None,
    }
}

/// The buttons pressed and released between two commands.
pub fn edges(prev: u32, now: u32) -> (u32, u32) {
    (now & !prev, prev & !now)
}

/// `maps/ts_dojo.bsp` -> `ts_dojo` (`pfnGetLevelName`'s form); a bare name stays.
pub fn map_name(level: &str) -> &str {
    let base = level.rsplit(['/', '\\']).next().unwrap_or(level);
    base.strip_suffix(".bsp").unwrap_or(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(e: &GameEvent) -> String {
        e.text.iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect()
    }

    #[test]
    fn hud_changes_become_events() {
        let a = Snapshot { health: 100, armor: 0, weapon: 0, team: 0 };
        let name = |id: i32| format!("gun {id}");
        assert!(changes(a, a, name).is_empty());
        let b = Snapshot { health: 80, weapon: 13, ..a };
        let ev = changes(a, b, name);
        assert_eq!(ev.iter().map(|e| e.kind).collect::<Vec<_>>(), [event::HEALTH, event::WEAPON]);
        assert_eq!((ev[0].a, ev[0].b), (80, 0));
        assert_eq!((ev[1].a, text(&ev[1])), (13, "gun 13".into()));
        let c = Snapshot { team: 2, ..b };
        assert_eq!(changes(b, c, name)[0].kind, event::TEAM);
    }

    #[test]
    fn messages_become_events() {
        let death = p::DeathMsg { killer: 2, victim: 1, weapon: "Glock-18".into() }.encode();
        let e = from_message(Msg::DeathMsg, &death, 1, "").unwrap();
        assert_eq!((e.kind, e.a, e.b, e.c, text(&e)), (event::DEATH, 2, 1, 1, "Glock-18".into()));
        assert_eq!(from_message(Msg::DeathMsg, &death, 3, "").unwrap().c, 0, "someone else's death");
        assert_eq!(from_message(Msg::DeathMsg, &[1], 1, ""), None, "malformed");
        assert_eq!(from_message(Msg::ResetHUD, &[0], 1, "").unwrap().kind, event::SPAWN);
        let m = from_message(Msg::InitHUD, &[], 1, "ts_dojo").unwrap();
        assert_eq!((m.kind, text(&m)), (event::MAP, "ts_dojo".into()));
        let r = from_message(Msg::RoundTime, &p::RoundTime { secs: 200, mode: 1 }.encode(), 1, "").unwrap();
        assert_eq!((r.kind, r.a, r.b), (event::ROUND, 200, 1));
        assert_eq!(from_message(Msg::SayText, &[], 1, ""), None);
    }

    #[test]
    fn long_text_is_cut() {
        let e = make(event::WEAPON, 0, 0, 0, &[b'x'; 100]);
        assert_eq!(text(&e).len(), NAME_LEN - 1);
        assert_eq!(e.text[NAME_LEN - 1], 0);
    }

    #[test]
    fn button_edges_and_map_names() {
        assert_eq!(edges(0b0011, 0b0110), (0b0100, 0b0001));
        assert_eq!(edges(5, 5), (0, 0));
        assert_eq!(map_name("maps/ts_dojo.bsp"), "ts_dojo");
        assert_eq!(map_name("maps\\x.bsp"), "x");
        assert_eq!(map_name(""), "");
    }
}
