//! Menus client extensions take over (port addition, issue #314; docs/implementation/
//! client-ext-api.md §12, `ts_client_ext_api`'s "HUD control and menus"). A block of a
//! `ShowMenu`, a `VGUIMenu` the port would show, or a `MOTD` is a takeover: the port keeps the
//! menu's messages and what it offers, checks the extension's answer ([`Takeovers::reply`])
//! against that, and gives the messages back to display when the menu fails open
//! ([`Takeovers::give_back`]). Pure, so it is unit tested; [`super`] feeds it the HUD's messages
//! and the extensions' verdicts.

use shared::protocol::{Msg, hud as p};
use ts_client_ext_api::{ExtId, INVALID, NOT_FOUND, menu, menu_closed};

/// What a taken-over menu lets its extension answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Offer {
    /// A text menu's keys (`ShowMenu`'s `slots`: bit 0 is key 1, bit 9 key 10).
    Keys(u16),
    /// The team menu (`VGUIMenu` 2): a team it lists, or auto-assign.
    Team,
    /// Only closing it: the map briefing, the MOTD window, the MOTD.
    Close,
}

/// The port's own answer, as its menu sends it ([`Takeovers::reply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// To the server: `menuselect N` (`CHudMenu::SelectMenuItem`).
    Server(String),
    /// The team menu's `jointeam N` (`CMenuHandler_StringCommandWatch`, a client command).
    Team(i32),
    /// Closed, nothing sent (Esc, cancel, OK).
    Close,
}

/// One taken-over menu.
#[derive(Clone, Debug, PartialEq)]
pub struct Takeover {
    /// A [`menu`] kind.
    pub kind: i32,
    pub ext: ExtId,
    /// Its messages so far, as the extension saw them, in order (a text menu and a MOTD come in
    /// chunks).
    chunks: Vec<(Msg, Vec<u8>)>,
    offer: Offer,
    /// The last chunk came (`ShowMenu`'s `more` 0, `MOTD`'s `last`).
    complete: bool,
    /// When `ShowMenu`'s display time runs out, milliseconds.
    deadline_ms: Option<f64>,
}

/// A takeover that ended without the extension's answer: who to tell, and why
/// (`ts_client_ext_api::menu_closed`).
pub type Closed = (ExtId, i32, i32);

/// A takeover given back ([`Takeovers::give_back`]): its extension, its kind and its messages, to
/// display in order.
pub type GivenBack = (ExtId, i32, Vec<(Msg, Vec<u8>)>);

/// What the port knows when a menu message comes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ctx {
    /// Teamplay (`gamemode` ≥ 1): the port shows the team menu only then (`ShowVGUIMenu`).
    pub teamplay: bool,
    pub now_ms: f64,
}

/// The [`menu`] kind of a message, if it is a menu.
pub fn kind_of(m: Msg) -> Option<i32> {
    match m {
        Msg::ShowMenu => Some(menu::TEXT),
        Msg::VguiMenu => Some(menu::VGUI),
        Msg::Motd => Some(menu::MOTD),
        _ => None,
    }
}

/// The takeovers in progress: at most one per kind.
#[derive(Clone, Debug, Default)]
pub struct Takeovers {
    list: Vec<Takeover>,
}

impl Takeovers {
    /// Whether nothing is taken over.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The open takeovers: extension and kind.
    pub fn open(&self) -> Vec<(ExtId, i32)> {
        self.list.iter().map(|t| (t.ext, t.kind)).collect()
    }

    fn end(&mut self, kind: i32, reason: i32, out: &mut Vec<Closed>) {
        if let Some(i) = self.list.iter().position(|t| t.kind == kind) {
            let t = self.list.remove(i);
            out.push((t.ext, t.kind, reason));
        }
    }

    /// Menu message `m` (`buf` as the blocking extension saw it) was blocked by `blocked_by`, or
    /// (`None`) is displayed by the port. Gives the takeovers it ended.
    pub fn message(&mut self, m: Msg, buf: &[u8], blocked_by: Option<ExtId>, cx: Ctx) -> Vec<Closed> {
        let Some(kind) = kind_of(m) else { return Vec::new() };
        let mut out = Vec::new();
        // A chunk of a takeover this extension is still receiving.
        let cont = |t: &Takeover| Some(t.ext) == blocked_by && t.kind == kind && !t.complete;
        match m {
            Msg::ShowMenu => {
                let Some(v) = p::ShowMenu::decode(buf) else { return out };
                if v.slots == 0 {
                    // The server closed the menu.
                    self.end(kind, menu_closed::EXPIRED, &mut out);
                    return out;
                }
                let deadline_ms = (v.time > 0).then_some(cx.now_ms + v.time as f64 * 1000.0);
                let complete = v.more == 0;
                if let Some(t) = self.list.iter_mut().find(|t| cont(t)) {
                    t.chunks.push((m, buf.to_vec()));
                    t.offer = Offer::Keys(v.slots);
                    t.complete = complete;
                    t.deadline_ms = deadline_ms;
                    return out;
                }
                self.end(kind, menu_closed::REPLACED, &mut out);
                if let Some(ext) = blocked_by {
                    let offer = Offer::Keys(v.slots);
                    self.list.push(Takeover { kind, ext, chunks: vec![(m, buf.to_vec())], offer, complete, deadline_ms });
                }
            }
            Msg::VguiMenu => {
                // `ShowVGUIMenu` (`0x10064050`) makes only these; the team menu only in teamplay.
                let offer = match buf.first() {
                    Some(2) if cx.teamplay => Offer::Team,
                    Some(4 | 5) => Offer::Close,
                    _ => return out,
                };
                self.end(kind, menu_closed::REPLACED, &mut out);
                if let Some(ext) = blocked_by {
                    self.list.push(Takeover { kind, ext, chunks: vec![(m, buf.to_vec())], offer, complete: true, deadline_ms: None });
                }
            }
            _ => {
                let Some(v) = p::Motd::decode(buf) else { return out };
                let complete = v.last != 0;
                if let Some(t) = self.list.iter_mut().find(|t| cont(t)) {
                    t.chunks.push((m, buf.to_vec()));
                    t.complete = complete;
                    return out;
                }
                self.end(kind, menu_closed::REPLACED, &mut out);
                if let Some(ext) = blocked_by {
                    let chunks = vec![(m, buf.to_vec())];
                    self.list.push(Takeover { kind, ext, chunks, offer: Offer::Close, complete, deadline_ms: None });
                }
            }
        }
        out
    }

    /// Extension `ext`'s answer `choice` to its takeover of `kind` (`teams`: how many the
    /// server's `TeamNames` lists): the port's answer, and the takeover ends; or `NOT_FOUND` /
    /// `INVALID` and it stays.
    pub fn reply(&mut self, ext: ExtId, kind: i32, choice: &str, teams: usize) -> Result<Answer, i32> {
        if kind_of_code(kind).is_none() {
            return Err(INVALID);
        }
        let i = self.list.iter().position(|t| t.ext == ext && t.kind == kind && t.complete).ok_or(NOT_FOUND)?;
        let choice = choice.trim();
        let answer = if choice.is_empty() {
            Answer::Close
        } else {
            let n: i32 = choice.parse().map_err(|_| INVALID)?;
            match self.list[i].offer {
                // `SelectMenuItem`: a key the menu has.
                Offer::Keys(slots) if (1..=10).contains(&n) && slots & (1 << (n - 1)) != 0 => Answer::Server(format!("menuselect {n}")),
                // `SlotInput` (`0x100665a0`): auto-assign, or a team with a visible button (the
                // menu has four).
                Offer::Team if n == 5 || (n >= 1 && n as usize <= teams.min(4)) => Answer::Team(n),
                _ => return Err(INVALID),
            }
        };
        self.list.remove(i);
        Ok(answer)
    }

    /// Takeovers whose `ShowMenu` display time ran out.
    pub fn expire(&mut self, now_ms: f64) -> Vec<Closed> {
        let mut out = Vec::new();
        self.list.retain(|t| {
            let over = t.deadline_ms.is_some_and(|d| now_ms >= d);
            if over {
                out.push((t.ext, t.kind, menu_closed::EXPIRED));
            }
            !over
        });
        out
    }

    /// `ResetHUD`, or the connection ended: every takeover ends.
    pub fn reset(&mut self) -> Vec<Closed> {
        self.list.drain(..).map(|t| (t.ext, t.kind, menu_closed::RESET)).collect()
    }

    /// The takeovers `pick` chooses fail open: they end, and their messages go back to be
    /// displayed, in order.
    pub fn give_back(&mut self, mut pick: impl FnMut(&Takeover) -> bool) -> Vec<GivenBack> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.list.len() {
            if pick(&self.list[i]) {
                let t = self.list.remove(i);
                out.push((t.ext, t.kind, t.chunks));
            } else {
                i += 1;
            }
        }
        out
    }
}

/// A [`menu`] kind code's message, if it is one.
fn kind_of_code(kind: i32) -> Option<Msg> {
    match kind {
        menu::TEXT => Some(Msg::ShowMenu),
        menu::VGUI => Some(Msg::VguiMenu),
        menu::MOTD => Some(Msg::Motd),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(slots: u16, time: i8, more: u8, text: &str) -> Vec<u8> {
        p::ShowMenu { slots, time, more, text: text.into() }.encode()
    }

    fn motd(last: u8, text: &str) -> Vec<u8> {
        p::Motd { last, text: text.into() }.encode()
    }

    const CX: Ctx = Ctx { teamplay: true, now_ms: 0.0 };

    #[test]
    fn a_text_menu_answers_only_its_keys() {
        let mut t = Takeovers::default();
        assert!(t.message(Msg::ShowMenu, &show(0b101, -1, 1, "1. a\n"), Some(7), CX).is_empty());
        assert_eq!(t.reply(7, menu::TEXT, "1", 0), Err(NOT_FOUND), "the rest of its text is still to come");
        t.message(Msg::ShowMenu, &show(0b101, -1, 0, "3. c"), Some(7), CX);
        assert_eq!(t.reply(8, menu::TEXT, "1", 0), Err(NOT_FOUND), "another extension's");
        assert_eq!(t.reply(7, menu::VGUI, "1", 0), Err(NOT_FOUND), "another kind");
        assert_eq!(t.reply(7, 9, "1", 0), Err(INVALID));
        assert_eq!(t.reply(7, menu::TEXT, "2", 0), Err(INVALID), "a key the menu doesn't have");
        assert_eq!(t.reply(7, menu::TEXT, "11", 0), Err(INVALID));
        assert_eq!(t.reply(7, menu::TEXT, "kill", 0), Err(INVALID));
        assert_eq!(t.reply(7, menu::TEXT, "3", 0), Ok(Answer::Server("menuselect 3".into())));
        assert_eq!(t.reply(7, menu::TEXT, "3", 0), Err(NOT_FOUND), "answered once");
        // Key 10 is bit 9; "" closes it unanswered.
        t.message(Msg::ShowMenu, &show(1 << 9, -1, 0, "0. x"), Some(7), CX);
        assert_eq!(t.reply(7, menu::TEXT, "10", 0), Ok(Answer::Server("menuselect 10".into())));
        t.message(Msg::ShowMenu, &show(1, -1, 0, "1. x"), Some(7), CX);
        assert_eq!(t.reply(7, menu::TEXT, "", 0), Ok(Answer::Close));
        assert!(t.is_empty());
    }

    #[test]
    fn the_team_menu_offers_the_listed_teams_and_auto_assign() {
        let mut t = Takeovers::default();
        t.message(Msg::VguiMenu, &[2], Some(1), CX);
        assert_eq!(t.reply(1, menu::VGUI, "3", 2), Err(INVALID), "two teams listed");
        assert_eq!(t.reply(1, menu::VGUI, "5", 2), Ok(Answer::Team(5)));
        t.message(Msg::VguiMenu, &[2], Some(1), CX);
        assert_eq!(t.reply(1, menu::VGUI, "4", 9), Ok(Answer::Team(4)), "the menu has four buttons");
        t.message(Msg::VguiMenu, &[2], Some(1), CX);
        assert_eq!(t.reply(1, menu::VGUI, "5 ", 0), Ok(Answer::Team(5)));
        // The briefing and the MOTD window only close; the team menu outside teamplay and other
        // ids the port never shows, so they aren't taken over.
        t.message(Msg::VguiMenu, &[5], Some(1), CX);
        assert_eq!(t.reply(1, menu::VGUI, "1", 4), Err(INVALID));
        assert_eq!(t.reply(1, menu::VGUI, "", 4), Ok(Answer::Close));
        t.message(Msg::VguiMenu, &[2], Some(1), Ctx { teamplay: false, ..CX });
        t.message(Msg::VguiMenu, &[3], Some(1), CX);
        assert!(t.is_empty());
    }

    #[test]
    fn a_motd_comes_in_chunks_and_only_closes() {
        let mut t = Takeovers::default();
        t.message(Msg::Motd, &motd(0, "Hello "), Some(2), CX);
        assert_eq!(t.reply(2, menu::MOTD, "", 0), Err(NOT_FOUND));
        t.message(Msg::Motd, &motd(1, "world"), Some(2), CX);
        let back = t.give_back(|_| true);
        assert_eq!(back, vec![(2, menu::MOTD, vec![(Msg::Motd, motd(0, "Hello ")), (Msg::Motd, motd(1, "world"))])]);
    }

    #[test]
    fn newer_menus_the_server_reset_and_time_end_takeovers() {
        let mut t = Takeovers::default();
        t.message(Msg::ShowMenu, &show(1, 5, 0, "a"), Some(1), Ctx { now_ms: 1000.0, ..CX });
        t.message(Msg::VguiMenu, &[2], Some(2), CX);
        // A newer text menu, displayed by the port (or another's takeover), replaces it.
        assert_eq!(t.message(Msg::ShowMenu, &show(1, -1, 0, "b"), None, CX), vec![(1, menu::TEXT, menu_closed::REPLACED)]);
        assert_eq!(t.open(), vec![(2, menu::VGUI)]);
        t.message(Msg::ShowMenu, &show(1, 5, 0, "c"), Some(1), Ctx { now_ms: 1000.0, ..CX });
        assert!(t.expire(5999.0).is_empty());
        assert_eq!(t.expire(6000.0), vec![(1, menu::TEXT, menu_closed::EXPIRED)], "its 5 s");
        t.message(Msg::ShowMenu, &show(1, -1, 0, "d"), Some(1), CX);
        assert_eq!(
            t.message(Msg::ShowMenu, &show(0, 0, 0, ""), None, CX),
            vec![(1, menu::TEXT, menu_closed::EXPIRED)],
            "the server closed it"
        );
        assert_eq!(t.reset(), vec![(2, menu::VGUI, menu_closed::RESET)]);
        assert!(t.is_empty());
        // Messages that aren't menus change nothing.
        assert!(t.message(Msg::SayText, b"\x01hi\0", Some(1), CX).is_empty() && t.is_empty());
    }

    #[test]
    fn failing_open_gives_the_messages_back_in_order() {
        let mut t = Takeovers::default();
        t.message(Msg::ShowMenu, &show(3, -1, 1, "1. a\n"), Some(4), CX);
        t.message(Msg::ShowMenu, &show(3, -1, 0, "2. b"), Some(4), CX);
        t.message(Msg::VguiMenu, &[4, b'm', 0], Some(5), CX);
        let back = t.give_back(|x| x.ext == 4);
        assert_eq!(back, vec![(4, menu::TEXT, vec![(Msg::ShowMenu, show(3, -1, 1, "1. a\n")), (Msg::ShowMenu, show(3, -1, 0, "2. b"))])]);
        assert_eq!(t.open(), vec![(5, menu::VGUI)]);
    }
}
