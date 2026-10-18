//! The client extensions' interception hooks (issue #295, port addition; client-ext-api.md §11,
//! `ts_client_ext_api`'s "Interception"): what each message and command lets a verdict do, the
//! checks on replacements, and the run of a message through the extensions with its holds. Pure
//! (the extensions are asked through a closure), so it is unit tested; `super` wires it to the
//! HUD's hooks and the extensions.
//!
//! The rules follow the webview specification's split (§7.6, §7.10): presentation can be
//! blocked, rewritten or held; state, effects, input buttons and the plugin approval path can't.

use shared::protocol::{Msg, hud as p};
use std::collections::VecDeque;
use ts_client_ext_api::{ExtId, HOLD_MS, HOLD_QUEUE, MESSAGE_MAX, allow};

/// An extension's answer, as the port reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Pass,
    Block,
    Replace(Vec<u8>),
    Defer,
}

/// What a verdict may do with message `m` ([`allow`] bits), or `None` for a message no hook sees:
/// `SrvSett` (the server's settings and version) and every port message after TS's 64 (`TSExt`,
/// the extensions' own channels, and `TSPkg`, the plugin offers, which the approval path owns,
/// client-plugins.md §7.3).
pub fn message_allow(m: Msg) -> Option<u32> {
    use allow::{BLOCK, DEFER, REPLACE};
    if m as usize >= shared::protocol::TS_MESSAGES {
        return None;
    }
    Some(match m {
        Msg::SrvSett | Msg::TsExt | Msg::TsPkg => return None,
        // Presentation: chat, the kill feed, text, HUD text, objectives, text menus.
        Msg::SayText | Msg::DeathMsg | Msg::TextMsg | Msg::HudText | Msg::TsMessage | Msg::Objective | Msg::ShowObj | Msg::ShowMenu => {
            BLOCK | REPLACE | DEFER
        }
        // The scoreboard award: the player it names can't change.
        Msg::TsPAward => BLOCK | DEFER,
        // Pickup notices (TS 3.0 draws none), VGUI menus and the MOTD.
        Msg::WeapPickup | Msg::AmmoPickup | Msg::ItemPickup | Msg::VguiMenu | Msg::Motd => BLOCK,
        // State and effects: observe only.
        _ => 0,
    })
}

/// Whether a replacement for message `m` (`orig` as it came) is one the port takes: it decodes
/// as the message, fits, and keeps who the message names. The reason when it isn't.
pub fn check_replacement(m: Msg, orig: &[u8], new: &[u8]) -> Result<(), &'static str> {
    if new.len() > MESSAGE_MAX {
        return Err("longer than a user message");
    }
    let same = |a: Option<u8>, b: Option<u8>| a.is_some() && a == b;
    let ok = match m {
        Msg::SayText => same(p::SayText::decode(orig).map(|v| v.client), p::SayText::decode(new).map(|v| v.client)),
        Msg::DeathMsg => {
            let who = |b: &[u8]| p::DeathMsg::decode(b).map(|d| (d.killer, d.victim));
            let (a, b) = (who(orig), who(new));
            a.is_some() && a == b
        }
        Msg::TsPAward => same(p::TsPAward::decode(orig).map(|v| v.client), p::TsPAward::decode(new).map(|v| v.client)),
        Msg::TextMsg => p::TextMsg::decode(new).is_some(),
        Msg::HudText => !new.is_empty() && new.ends_with(&[0]),
        Msg::TsMessage => p::TsMessage::decode(new).is_some(),
        Msg::Objective => p::Objective::decode(new).is_some(),
        Msg::ShowObj => new.len() == 2 && p::ShowObj::decode(new).is_some(),
        Msg::ShowMenu => p::ShowMenu::decode(new).is_some(),
        _ => false,
    };
    if ok { Ok(()) } else { Err("not a valid message of its kind, or it names someone else") }
}

/// Commands whose verdicts count for nothing: input buttons (`+attack`, `-use`, ...: blocking
/// them would make a triggerbot), the dev aids, and the plugin approval path's commands
/// (client-plugins.md §7.3, §8: `cl_plugins*`, `tsrs_*`). Extensions' own commands are protected
/// too ([`command_allow`]'s `extension`).
fn protected_command(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with('+')
        || n.starts_with('-')
        || n.starts_with("ts_dev_")
        || n == "ts_hud_selftest"
        || n.starts_with("cl_plugins")
        || n.starts_with("tsrs_")
}

/// What a `command_pre` verdict may do with client command `name`: block it, unless it is
/// protected or an extension's own (`extension`).
pub fn command_allow(name: &str, extension: bool) -> u32 {
    if extension || protected_command(name) { 0 } else { allow::BLOCK }
}

/// What a `server_cmd_pre` verdict may do with a command the port sends: block or rewrite it,
/// except the plugin approval path's replies (`tspkg`, client-plugins.md §6.5) and the voice
/// manager's state (`vban`, `VModEnable`).
pub fn server_cmd_allow(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    if n == "tspkg" || n == "vban" || n == "vmodenable" { 0 } else { allow::BLOCK | allow::REPLACE }
}

/// A replacement server command line: it keeps the command's name and runs no second command.
pub fn check_command_line(orig_name: &str, new: &str) -> Result<(), &'static str> {
    if new.len() >= ts_client_ext_api::COMMAND_MAX {
        return Err("too long");
    }
    if new.bytes().any(|b| b == b';' || b == b'\n' || b == b'\r' || b == 0) {
        return Err("has a command separator");
    }
    if !tokenize(new).first().is_some_and(|n| n.eq_ignore_ascii_case(orig_name)) {
        return Err("changes the command's name");
    }
    Ok(())
}

/// A line's words as the engines tokenise a command: whitespace apart, a quoted run one word
/// (the quotes dropped).
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        let mut w = String::new();
        if c == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                w.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                w.push(c);
                chars.next();
            }
        }
        out.push(w);
    }
    out
}

/// One question to an extension about a message.
pub struct Ask<'a> {
    pub kind: Msg,
    pub ext: ExtId,
    pub data: &'a [u8],
    pub allow: u32,
    /// For a [`Verdict::Defer`]; 0 when it isn't allowed.
    pub token: u32,
}

/// How a run through the extensions ended.
#[derive(Debug, PartialEq)]
pub enum Run {
    /// Displayed with this payload, or (`None`) blocked.
    Done(Option<Vec<u8>>),
    /// An extension holds it.
    Held(Wait),
}

/// A held message: who holds it and who comes after.
#[derive(Debug, PartialEq)]
pub struct Wait {
    pub token: u32,
    pub ext: ExtId,
    pub rest: Vec<ExtId>,
    pub payload: Vec<u8>,
    pub since_ms: f64,
}

/// A message waiting to be displayed, in arrival order.
struct Entry {
    orig: Vec<u8>,
    state: Slot,
}

enum Slot {
    Waiting(Wait),
    /// Its run is over: display this, or (`None`) nothing.
    Ready(Option<Vec<u8>>),
}

/// The extensions' holds, per message kind, and the tokens.
#[derive(Default)]
pub struct Holds {
    kinds: Vec<(Msg, VecDeque<Entry>)>,
    next_token: u32,
    /// Menus an extension blocked since the last [`Holds::take_blocks`]: the message, the
    /// extension, the payload as it saw it (the takeovers, issue #314, `super::takeover`).
    blocks: Vec<(Msg, ExtId, Vec<u8>)>,
}

/// What [`Holds::message`] tells the HUD.
#[derive(Debug, PartialEq)]
pub enum Dispatch {
    /// Nothing changed: handle the message as usual.
    Whole,
    /// Handle its state now and display this (`None`: nothing yet, or never).
    Split(Option<Vec<u8>>),
}

/// The asking closure: `None` when that extension no longer takes part (suspended, gone).
pub type AskFn<'a> = dyn FnMut(&Ask) -> Option<Verdict> + 'a;
/// The warning closure (a console line).
pub type WarnFn<'a> = dyn FnMut(ExtId, String) + 'a;

impl Holds {
    fn token(&mut self) -> u32 {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.next_token
    }

    fn queue(&mut self, m: Msg) -> &mut VecDeque<Entry> {
        let i = match self.kinds.iter().position(|(k, _)| *k == m) {
            Some(i) => i,
            None => {
                self.kinds.push((m, VecDeque::new()));
                self.kinds.len() - 1
            }
        };
        &mut self.kinds[i].1
    }

    /// Whether anything is held or waiting.
    pub fn is_empty(&self) -> bool {
        self.kinds.iter().all(|(_, q)| q.is_empty())
    }

    /// The menus extensions blocked since the last call, in order.
    pub fn take_blocks(&mut self) -> Vec<(Msg, ExtId, Vec<u8>)> {
        std::mem::take(&mut self.blocks)
    }

    /// Extension `ext` blocked message `m` (`payload` as it saw it): a takeover if it is a menu.
    fn blocked(&mut self, m: Msg, ext: ExtId, payload: &[u8]) {
        if super::takeover::kind_of(m).is_some() {
            self.blocks.push((m, ext, payload.to_vec()));
        }
    }

    /// Runs `payload` (message `m`, `orig` as it came) through `exts` in order.
    fn run(
        &mut self,
        (m, orig): (Msg, &[u8]),
        mut payload: Vec<u8>,
        exts: &[ExtId],
        now_ms: f64,
        ask: &mut AskFn,
        warn: &mut WarnFn,
    ) -> Run {
        let Some(allowed) = message_allow(m) else { return Run::Done(Some(payload)) };
        for (i, &ext) in exts.iter().enumerate() {
            let token = if allowed & allow::DEFER != 0 { self.token() } else { 0 };
            let Some(v) = ask(&Ask { kind: m, ext, data: &payload, allow: allowed, token }) else { continue };
            let name = m.name().to_string_lossy();
            match v {
                Verdict::Pass => {}
                Verdict::Block if allowed & allow::BLOCK != 0 => {
                    self.blocked(m, ext, &payload);
                    return Run::Done(None);
                }
                Verdict::Replace(new) if allowed & allow::REPLACE != 0 => match check_replacement(m, orig, &new) {
                    Ok(()) => payload = new,
                    Err(why) => {
                        warn(ext, format!("its {name} replacement is {why}: blocked"));
                        return Run::Done(None);
                    }
                },
                Verdict::Defer if allowed & allow::DEFER != 0 => {
                    return Run::Held(Wait { token, ext, rest: exts[i + 1..].to_vec(), payload, since_ms: now_ms });
                }
                other => warn(ext, format!("{name} doesn't take {other:?}: passed")),
            }
        }
        Run::Done(Some(payload))
    }

    /// A message from the server, run through the extensions `exts` (in load order).
    pub fn message(&mut self, m: Msg, buf: &[u8], exts: &[ExtId], now_ms: f64, ask: &mut AskFn, warn: &mut WarnFn) -> Dispatch {
        let run = self.run((m, buf), buf.to_vec(), exts, now_ms, ask, warn);
        let behind = !self.queue(m).is_empty();
        match run {
            Run::Done(Some(p)) if !behind && p == buf => Dispatch::Whole,
            Run::Done(d) if !behind => Dispatch::Split(d),
            run => {
                let state = match run {
                    Run::Done(d) => Slot::Ready(d),
                    Run::Held(w) => Slot::Waiting(w),
                };
                let q = self.queue(m);
                q.push_back(Entry { orig: buf.to_vec(), state });
                if q.len() > HOLD_QUEUE {
                    // The oldest passes as it is now, unasked.
                    if let Some(Entry { state: s @ Slot::Waiting(_), .. }) = q.front_mut() {
                        let Slot::Waiting(w) = std::mem::replace(s, Slot::Ready(None)) else { unreachable!() };
                        *s = Slot::Ready(Some(w.payload));
                    }
                }
                Dispatch::Split(None)
            }
        }
    }

    /// Settles the waiting entries `pick` chooses (with the verdict it gives), goes on with their
    /// runs, and returns what is now ready to display, in order: message, payload.
    fn settle(
        &mut self,
        now_ms: f64,
        ask: &mut AskFn,
        warn: &mut WarnFn,
        pick: &mut dyn FnMut(&Wait) -> Option<Verdict>,
    ) -> Vec<(Msg, Vec<u8>)> {
        let mut out = Vec::new();
        for k in 0..self.kinds.len() {
            let m = self.kinds[k].0;
            for i in 0..self.kinds[k].1.len() {
                let Slot::Waiting(w) = &self.kinds[k].1[i].state else { continue };
                let Some(v) = pick(w) else { continue };
                let Slot::Waiting(w) = std::mem::replace(&mut self.kinds[k].1[i].state, Slot::Ready(None)) else { unreachable!() };
                let orig = std::mem::take(&mut self.kinds[k].1[i].orig);
                let payload = match v {
                    Verdict::Block => {
                        self.blocked(m, w.ext, &w.payload);
                        None
                    }
                    Verdict::Replace(new) => match check_replacement(m, &orig, &new) {
                        Ok(()) => Some(new),
                        Err(why) => {
                            warn(w.ext, format!("its {} replacement is {why}: blocked", m.name().to_string_lossy()));
                            None
                        }
                    },
                    _ => Some(w.payload),
                };
                let state = match payload {
                    None => Slot::Ready(None),
                    Some(p) => match self.run((m, &orig), p, &w.rest, now_ms, ask, warn) {
                        Run::Done(d) => Slot::Ready(d),
                        Run::Held(w) => Slot::Waiting(w),
                    },
                };
                self.kinds[k].1[i] = Entry { orig, state };
            }
            let q = &mut self.kinds[k].1;
            while let Some(Entry { state: Slot::Ready(_), .. }) = q.front() {
                if let Some(Entry { state: Slot::Ready(Some(p)), .. }) = q.pop_front() {
                    out.push((m, p));
                }
            }
        }
        out
    }

    /// An extension's release of its hold `token` with `v` (`Pass`, `Block` or `Replace`). `None`
    /// if it holds no such message.
    pub fn release(
        &mut self,
        ext: ExtId,
        token: u32,
        v: Verdict,
        now_ms: f64,
        ask: &mut AskFn,
        warn: &mut WarnFn,
    ) -> Option<Vec<(Msg, Vec<u8>)>> {
        let held = self.kinds.iter().flat_map(|(_, q)| q).any(|e| matches!(&e.state, Slot::Waiting(w) if w.ext == ext && w.token == token));
        if !held {
            return None;
        }
        let mut v = Some(v);
        Some(self.settle(now_ms, ask, warn, &mut |w| if w.ext == ext && w.token == token { v.take() } else { None }))
    }

    /// Holds older than [`HOLD_MS`] pass (fail open).
    pub fn tick(&mut self, now_ms: f64, ask: &mut AskFn, warn: &mut WarnFn) -> Vec<(Msg, Vec<u8>)> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut late = Vec::new();
        let out = self.settle(now_ms, ask, warn, &mut |w| {
            (now_ms - w.since_ms >= HOLD_MS as f64).then(|| {
                late.push(w.ext);
                Verdict::Pass
            })
        });
        for ext in late {
            warn(ext, format!("held a message over {HOLD_MS} ms: passed"));
        }
        out
    }

    /// Extension `ext` stops taking part (suspended): its holds pass.
    pub fn drop_ext(&mut self, ext: ExtId, now_ms: f64, ask: &mut AskFn, warn: &mut WarnFn) -> Vec<(Msg, Vec<u8>)> {
        self.settle(now_ms, ask, warn, &mut |w| (w.ext == ext).then_some(Verdict::Pass))
    }

    /// The connection ended: nothing held is displayed.
    pub fn clear(&mut self) {
        self.kinds.clear();
        self.blocks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn say(client: u8, text: &str) -> Vec<u8> {
        p::SayText { client, text: text.into() }.encode()
    }

    fn death(killer: u8, victim: u8, weapon: &str) -> Vec<u8> {
        p::DeathMsg { killer, victim, weapon: weapon.into() }.encode()
    }

    fn quiet() -> impl FnMut(ExtId, String) {
        |_, _| {}
    }

    /// Blocks of menus are kept for the takeovers, with who blocked them and the payload as that
    /// extension saw it; other blocks aren't.
    #[test]
    fn menu_blocks_are_recorded() {
        let mut h = Holds::default();
        let menu = p::ShowMenu { slots: 1, time: -1, more: 0, text: "1. x".into() }.encode();
        let mut ask = |a: &Ask| Some(if a.ext == 2 { Verdict::Block } else { Verdict::Pass });
        assert_eq!(h.message(Msg::ShowMenu, &menu, &[1, 2, 3], 0.0, &mut ask, &mut quiet()), Dispatch::Split(None));
        h.message(Msg::SayText, &say(1, "hi"), &[1, 2], 0.0, &mut ask, &mut quiet());
        assert_eq!(h.take_blocks(), vec![(Msg::ShowMenu, 2, menu.clone())]);
        assert!(h.take_blocks().is_empty());
        // A deferred menu released as a block counts too.
        let mut defer = |a: &Ask| Some(if a.ext == 1 { Verdict::Defer } else { Verdict::Pass });
        h.message(Msg::ShowMenu, &menu, &[1], 0.0, &mut defer, &mut quiet());
        let token = h.next_token;
        assert!(h.release(1, token, Verdict::Block, 1.0, &mut defer, &mut quiet()).is_some());
        assert_eq!(h.take_blocks(), vec![(Msg::ShowMenu, 1, menu)]);
    }

    #[test]
    fn what_each_message_allows() {
        assert_eq!(message_allow(Msg::SrvSett), None);
        assert_eq!(message_allow(Msg::TsExt), None);
        assert_eq!(message_allow(Msg::TsPkg), None);
        assert_eq!(message_allow(Msg::SayText), Some(allow::BLOCK | allow::REPLACE | allow::DEFER));
        assert_eq!(message_allow(Msg::TsPAward), Some(allow::BLOCK | allow::DEFER));
        assert_eq!(message_allow(Msg::WeapPickup), Some(allow::BLOCK));
        for state in
            [Msg::CurWeapon, Msg::Health, Msg::TsHealth, Msg::ScoreInfo, Msg::ResetHUD, Msg::TsFade, Msg::ScreenFade, Msg::TsSlowMo]
        {
            assert_eq!(message_allow(state), Some(0), "{state:?} is observe-only");
        }
    }

    #[test]
    fn replacements_keep_who_and_decode() {
        let orig = death(2, 3, "glock");
        assert_eq!(check_replacement(Msg::DeathMsg, &orig, &death(2, 3, "a spoon")), Ok(()));
        assert!(check_replacement(Msg::DeathMsg, &orig, &death(4, 3, "glock")).is_err(), "a forged killer");
        assert!(check_replacement(Msg::SayText, &say(1, "hi"), &say(2, "hi")).is_err(), "a forged sender");
        assert_eq!(check_replacement(Msg::SayText, &say(1, "hi"), &say(1, "hello")), Ok(()));
        assert!(check_replacement(Msg::SayText, &say(1, "hi"), &[0u8; 200]).is_err());
        assert!(check_replacement(Msg::ShowObj, b"\x01\x01", b"\x01").is_err());
        assert!(check_replacement(Msg::HudText, b"a\0", b"no terminator").is_err());
        assert!(check_replacement(Msg::CurWeapon, b"\x01\x02\x03", b"\x01\x02\x03").is_err(), "state takes none");
    }

    #[test]
    fn commands() {
        assert_eq!(command_allow("slot3", false), allow::BLOCK);
        assert_eq!(command_allow("+attack", false), 0);
        assert_eq!(command_allow("-USE", false), 0);
        assert_eq!(command_allow("cl_plugins_menu", false), 0);
        assert_eq!(command_allow("tsrs_reconnect", false), 0);
        assert_eq!(command_allow("ts_dev_key", false), 0);
        assert_eq!(command_allow("webview_toggle", true), 0, "an extension's own");
        assert_eq!(server_cmd_allow("menuselect"), allow::BLOCK | allow::REPLACE);
        assert_eq!(server_cmd_allow("tspkg"), 0);
        assert_eq!(server_cmd_allow("VModEnable"), 0);
        assert_eq!(check_command_line("menuselect", "menuselect 4"), Ok(()));
        assert!(check_command_line("menuselect", "menuselect 4; kill").is_err());
        assert!(check_command_line("menuselect", "kill").is_err());
        assert!(check_command_line("weapon_5", "weapon_5\nkill").is_err());
        assert_eq!(tokenize(r#"  say "hello there"  x "#), ["say", "hello there", "x"]);
        assert_eq!(tokenize(""), Vec::<String>::new());
    }

    /// Extension 1 rewrites, extension 2 blocks: the run stops at the block. With 2 passing, the
    /// rewrite is displayed.
    #[test]
    fn order_rewrites_accumulate_and_a_block_ends_the_run() {
        let mut h = Holds::default();
        let orig = say(1, "hi");
        let mut seen = Vec::new();
        let mut ask = |a: &Ask| {
            seen.push((a.ext, a.data.to_vec()));
            Some(if a.ext == 1 { Verdict::Replace(say(1, "HI")) } else { Verdict::Block })
        };
        assert_eq!(h.message(Msg::SayText, &orig, &[1, 2, 3], 0.0, &mut ask, &mut quiet()), Dispatch::Split(None));
        assert_eq!(seen, [(1, orig.clone()), (2, say(1, "HI"))], "3 never asked, 2 saw 1's rewrite");
        let mut ask = |a: &Ask| Some(if a.ext == 1 { Verdict::Replace(say(1, "HI")) } else { Verdict::Pass });
        assert_eq!(h.message(Msg::SayText, &orig, &[1, 2], 0.0, &mut ask, &mut quiet()), Dispatch::Split(Some(say(1, "HI"))));
        let mut pass = |_: &Ask| Some(Verdict::Pass);
        assert_eq!(h.message(Msg::SayText, &orig, &[1, 2], 0.0, &mut pass, &mut quiet()), Dispatch::Whole);
    }

    #[test]
    fn observe_only_messages_ignore_verdicts() {
        let mut h = Holds::default();
        let mut warned = 0;
        let mut ask = |a: &Ask| {
            assert_eq!((a.allow, a.token), (0, 0));
            Some(Verdict::Block)
        };
        let d = h.message(Msg::CurWeapon, &[1, 2, 3], &[1], 0.0, &mut ask, &mut |_, _| warned += 1);
        assert_eq!(d, Dispatch::Whole);
        assert_eq!(warned, 1);
    }

    #[test]
    fn a_bad_replacement_blocks() {
        let mut h = Holds::default();
        let mut warned = Vec::new();
        let mut ask = |_: &Ask| Some(Verdict::Replace(death(9, 3, "x")));
        let d = h.message(Msg::DeathMsg, &death(2, 3, "glock"), &[1], 0.0, &mut ask, &mut |e, w| warned.push((e, w)));
        assert_eq!(d, Dispatch::Split(None));
        assert!(warned[0].1.contains("names someone else"), "{warned:?}");
    }

    /// A held message holds back later ones of its kind (not other kinds); release goes on with
    /// the next extension; display comes in arrival order.
    #[test]
    fn holds_keep_arrival_order() {
        let mut h = Holds::default();
        let mut tokens = Vec::new();
        let mut ask = |a: &Ask| {
            if a.ext == 1 && a.data == say(1, "first").as_slice() {
                tokens.push(a.token);
                Some(Verdict::Defer)
            } else {
                Some(Verdict::Pass)
            }
        };
        assert_eq!(h.message(Msg::SayText, &say(1, "first"), &[1, 2], 0.0, &mut ask, &mut quiet()), Dispatch::Split(None));
        assert_eq!(
            h.message(Msg::SayText, &say(1, "second"), &[1, 2], 1.0, &mut ask, &mut quiet()),
            Dispatch::Split(None),
            "queued behind"
        );
        assert_eq!(
            h.message(Msg::TextMsg, &p::TextMsg { dest: 2, strings: vec!["x".into()] }.encode(), &[1, 2], 1.0, &mut ask, &mut quiet()),
            Dispatch::Whole
        );
        let t = tokens[0];
        assert_ne!(t, 0);
        let asked_after = std::cell::RefCell::new(Vec::new());
        let mut ask2 = |a: &Ask| {
            asked_after.borrow_mut().push(a.ext);
            Some(Verdict::Replace(say(1, "FIRST")))
        };
        assert_eq!(h.release(1, t + 1, Verdict::Pass, 2.0, &mut ask2, &mut quiet()), None, "a token it doesn't hold");
        assert_eq!(h.release(2, t, Verdict::Pass, 2.0, &mut ask2, &mut quiet()), None, "another extension's hold");
        let out = h.release(1, t, Verdict::Pass, 2.0, &mut ask2, &mut quiet()).unwrap();
        assert_eq!(*asked_after.borrow(), [2], "the run went on with extension 2");
        assert_eq!(out, [(Msg::SayText, say(1, "FIRST")), (Msg::SayText, say(1, "second"))]);
        assert!(h.is_empty());
        assert_eq!(h.release(1, t, Verdict::Pass, 3.0, &mut ask2, &mut quiet()), None, "released once");
    }

    #[test]
    fn holds_fail_open_and_end_with_suspension() {
        let mut h = Holds::default();
        let mut defer = |a: &Ask| Some(if a.ext == 1 { Verdict::Defer } else { Verdict::Pass });
        h.message(Msg::DeathMsg, &death(1, 2, "a"), &[1], 0.0, &mut defer, &mut quiet());
        assert_eq!(h.tick(HOLD_MS as f64 - 1.0, &mut defer, &mut quiet()), []);
        let mut warned = 0;
        assert_eq!(h.tick(HOLD_MS as f64, &mut defer, &mut |_, _| warned += 1), [(Msg::DeathMsg, death(1, 2, "a"))]);
        assert_eq!(warned, 1);
        h.message(Msg::DeathMsg, &death(1, 3, "b"), &[1, 2], 5000.0, &mut defer, &mut quiet());
        assert_eq!(h.drop_ext(2, 5000.0, &mut defer, &mut quiet()), [], "not 2's hold");
        assert_eq!(h.drop_ext(1, 5000.0, &mut defer, &mut quiet()), [(Msg::DeathMsg, death(1, 3, "b"))]);
        h.message(Msg::DeathMsg, &death(1, 4, "c"), &[1], 6000.0, &mut defer, &mut quiet());
        let mut blocked = |_: &Ask| Some(Verdict::Pass);
        let t = 4; // the fourth token handed out (one per ask that allows a hold)
        assert_eq!(h.release(1, t, Verdict::Block, 6001.0, &mut blocked, &mut quiet()), Some(vec![]), "released as a block");
        h.message(Msg::DeathMsg, &death(1, 5, "d"), &[1], 7000.0, &mut defer, &mut quiet());
        h.clear();
        assert!(h.is_empty());
    }

    #[test]
    fn a_full_queue_lets_the_oldest_pass() {
        let mut h = Holds::default();
        let mut defer = |_: &Ask| Some(Verdict::Defer);
        for i in 0..=HOLD_QUEUE {
            h.message(Msg::SayText, &say(1, &format!("{i}")), &[1], 0.0, &mut defer, &mut quiet());
        }
        // The oldest is ready now but shows only with the next settle (a release or a tick).
        let out = h.tick(1.0, &mut defer, &mut quiet());
        assert_eq!(out, [(Msg::SayText, say(1, "0"))]);
    }
}
