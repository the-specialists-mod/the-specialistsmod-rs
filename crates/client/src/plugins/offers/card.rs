//! The in-game cards (client-plugins.md §7.2, §7.3, §13.1): a server's offer, an update, the
//! port version a server needs (op 5), and "restart to finish" after an update of a loaded plugin.
//!
//! They're drawn in the game's frame, over everything (`HUD_Redraw`, after the HUD, the menus
//! and the extensions, which get no `redraw` while a card is up), with the engine's blended fills
//! and console font, and they take the virtual cursor while open. Not SDL card windows (the ones
//! restart and reconnect shows at the main menu, `crate::relaunch::card`): those stop the engine's
//! frames while open, and a player reading an offer card on a server would time out.
//!
//! This module holds the contents, the layout and the click handling, which the tests drive;
//! `super` draws and routes input. The rules (§7.3): the approving buttons take only a left
//! click (press and release on the button), stay disabled for [`super::super::click::ARM_DELAY`]
//! after the card appears or changes, and no key presses them; Escape (and Enter) choose the
//! default, which is never an approval.

use super::flow::{CardInfo, Update};
use plugin_pkg::{Platform, Trust, VersionChange};
use shared::protocol::tspkg::Offer;
use std::borrow::Cow;

/// What a button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// "Install and allow" (`scope = "always"`).
    Install,
    /// "Allow on this server only" (`scope = "server"`).
    ServerOnly,
    /// "Not now" / "Later" / "Cancel": the default.
    Later,
    /// "Never from this server".
    Never,
    /// Shift + "Never from this server": "Never", everywhere.
    NeverEverywhere,
    /// Op 5's card: restart through the launcher, which asks about the update.
    Update,
    /// "Restart now" (an update that loads at the next start).
    Restart,
}

/// What a card's button does, for the layout, the clicks and the drawing: the offer cards'
/// [`Action`], and the plugins panel's (`crate::plugins::menu`), which shares them.
pub trait Act: Copy + PartialEq {
    /// Whether the button approves (or otherwise writes the store), so it's click-only and armed
    /// late (§7.3).
    fn approves(self) -> bool;
}

impl Act for Action {
    fn approves(self) -> bool {
        Action::approves(self)
    }
}

impl Action {
    /// Whether the button approves something, so it's click-only and armed late (§7.3).
    pub fn approves(self) -> bool {
        matches!(self, Action::Install | Action::ServerOnly | Action::Update | Action::Restart)
    }
    pub fn name(self) -> &'static str {
        match self {
            Action::Install => "install",
            Action::ServerOnly => "server",
            Action::Later => "later",
            Action::Never => "never",
            Action::NeverEverywhere => "never_all",
            Action::Update => "update",
            Action::Restart => "restart",
        }
    }
    pub fn parse(s: &str) -> Option<Action> {
        [Action::Install, Action::ServerOnly, Action::Later, Action::Never, Action::NeverEverywhere, Action::Update, Action::Restart]
            .into_iter()
            .find(|a| a.name() == s.trim())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Offer,
    Update,
    Port,
    Restart,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Offer => "offer",
            Kind::Update => "update",
            Kind::Port => "port",
            Kind::Restart => "restart",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Title,
    Normal,
    Dim,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button<A = Action> {
    pub label: Cow<'static, str>,
    pub action: A,
    /// With Shift held: another label and action ("Never").
    pub shift: Option<(&'static str, A)>,
    /// A row of its own, as wide as the card (the plugins panel's list); its label is cut to fit.
    pub wide: bool,
}

impl<A: Act> Button<A> {
    pub fn new(label: impl Into<Cow<'static, str>>, action: A) -> Button<A> {
        Button { label: label.into(), action, shift: None, wide: false }
    }
    pub fn wide(label: impl Into<Cow<'static, str>>, action: A) -> Button<A> {
        Button { wide: true, ..Button::new(label, action) }
    }
    pub fn current(&self, shift: bool) -> (&str, A) {
        match self.shift {
            Some(s) if shift => s,
            _ => (&self.label, self.action),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub kind: Kind,
    /// The plugin id (`tsrs` for the port).
    pub id: String,
    pub lines: Vec<Line>,
    pub buttons: Vec<Button>,
}

impl Card {
    /// What Escape, Enter and every key choose: never an approval.
    pub fn default_action(&self) -> Action {
        Action::Later
    }

    /// One line for the e2e hook's record and the console.
    pub fn summary(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" / ")
    }

    pub fn labels(&self) -> Vec<&str> {
        self.buttons.iter().map(|b| b.label.as_ref()).collect()
    }
}

pub fn line(text: impl Into<String>, tone: Tone) -> Line {
    Line { text: text.into(), tone }
}

/// The native-code line every approval shows (§7.1).
pub const NATIVE: &str = "This is a program that runs inside the game with your account's permissions.";

/// Bytes, for people.
pub fn size_text(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} bytes"),
    }
}

/// The server line: "<name> (<address>)", the address alone without a name (§7.3 "Clear source").
pub fn server_text(name: &str, address: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    if name.trim().is_empty() { address.to_owned() } else { format!("{} ({address})", name.trim()) }
}

/// The offer or update card (§7.2, §6.4).
pub fn offer(o: &Offer, info: &CardInfo, platform: Platform, server: &str) -> Card {
    let m = &info.checked.manifest;
    let mut lines = vec![line(format!("{server} offers a client plugin"), Tone::Title), line("", Tone::Normal)];
    let (kind, title) = match &info.update {
        Some(u) => (Kind::Update, format!("Update: {} {} -> {}", m.name, u.old, m.version)),
        None => (Kind::Offer, format!("{} {} ({})", m.name, m.version, m.id)),
    };
    lines.push(line(title, Tone::Normal));
    let trust_tone = if info.checked.trust.trust == Trust::Unsigned { Tone::Warn } else { Tone::Normal };
    lines.push(line(info.checked.trust.to_string(), trust_tone));
    if let Some(h) = &m.homepage {
        lines.push(line(h.clone(), Tone::Dim));
    }
    if !m.description.is_empty() {
        lines.push(line(m.description.clone(), Tone::Normal));
    }
    let download = if info.installed.is_some() { "already downloaded".to_owned() } else { size_text(u64::from(o.size)) };
    lines.push(line(format!("Download {download}, installed {}", size_text(m.installed_size(platform))), Tone::Dim));
    let mut declares = Vec::new();
    if !m.declares.processes.is_empty() {
        let names: Vec<&str> = m.declares.processes.iter().map(|p| p.rsplit('/').next().unwrap_or(p)).collect();
        declares.push(format!("starts programs: {}", names.join(", ")));
    }
    if m.declares.network {
        declares.push("uses the network".into());
    }
    if m.declares.data {
        declares.push("stores data".into());
    }
    if !declares.is_empty() {
        lines.push(line(format!("It says it {}.", declares.join(", ")), Tone::Normal));
    }
    if let Some(u) = &info.update {
        lines.extend(update_lines(u));
    }
    if o.required() {
        lines.push(line("Required by this server.", Tone::Warn));
    }
    if !o.note.is_empty() {
        lines.push(line(format!("The server says: {}", o.note), Tone::Normal));
    }
    if info.installed.is_some() {
        lines.push(line("Already installed: approved for other servers only.", Tone::Dim));
    }
    lines.push(line("", Tone::Normal));
    lines.push(line(NATIVE, Tone::Normal));
    lines.push(line("\"This server only\": it loads on this server, and stays loaded until the game exits.", Tone::Dim));
    let buttons = vec![
        Button::new(if kind == Kind::Update { "Install update" } else { "Install and allow" }, Action::Install),
        Button::new("Allow on this server only", Action::ServerOnly),
        Button::new("Not now", Action::Later),
        Button { shift: Some(("Never (everywhere)", Action::NeverEverywhere)), ..Button::new("Never from this server", Action::Never) },
    ];
    Card { kind, id: o.id.clone(), lines, buttons }
}

fn update_lines(u: &Update) -> Vec<Line> {
    let mut out = Vec::new();
    match u.change {
        VersionChange::Downgrade => {
            out.push(line(format!("This is an OLDER version than the approved {}: a downgrade.", u.old), Tone::Warn));
        }
        VersionChange::Same | VersionChange::Rebuild => out.push(line("The same version, rebuilt: its files changed.", Tone::Warn)),
        VersionChange::Upgrade => {}
    }
    if u.key_changed {
        out.push(line("The publisher key changed: treat it as a different publisher.", Tone::Warn));
    }
    if let Some(s) = &u.summary {
        let declares =
            if s.declares_changed.is_empty() { String::new() } else { format!("; declares changed: {}", s.declares_changed.join(", ")) };
        out.push(line(format!("{} file(s) changed{declares}.", s.files_changed), Tone::Normal));
    }
    out.push(line("A plugin that's loaded now updates at the next start.", Tone::Dim));
    out
}

/// Op 5's card (§13.1, §13.5): the port version the server needs. The button restarts through
/// the launcher, which asks again before it installs anything.
pub fn port(server: &str, requirement: &str, url: &str, ours: &str) -> Card {
    let mut lines = vec![
        line(format!("{server} needs another version of The Specialists port"), Tone::Title),
        line("", Tone::Normal),
        line(format!("It needs {requirement}; this game is {ours}."), Tone::Normal),
        line("Restarting goes through the TS launcher, which shows the update and asks first,", Tone::Normal),
        line("then reconnects. Without a launcher the game tells you where to get it.", Tone::Normal),
    ];
    if !url.is_empty() {
        lines.push(line(format!("The server says to get it from {url}"), Tone::Dim));
    }
    lines.push(line("The server disconnects players with another version shortly.", Tone::Dim));
    Card {
        kind: Kind::Port,
        id: shared::protocol::tspkg::PORT_ID.into(),
        lines,
        buttons: vec![Button::new("Restart to update", Action::Update), Button::new("Not now", Action::Later)],
    }
}

/// "Restart to finish" (§9 "Update while loaded").
pub fn restart(server: &str, name: &str, version: &str, id: &str) -> Card {
    Card {
        kind: Kind::Restart,
        id: id.into(),
        lines: vec![
            line(format!("{name} {version} is installed and approved"), Tone::Title),
            line("", Tone::Normal),
            line("The version loaded now stays until the game exits: restart to finish.", Tone::Normal),
            line(format!("Restarting reconnects to {server}."), Tone::Dim),
        ],
        buttons: vec![Button::new("Restart now", Action::Restart), Button::new("Later", Action::Later)],
    }
}

// ---- layout and clicks ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// Where everything goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub panel: Rect,
    /// (x, y, text, tone), wrapped.
    pub lines: Vec<(i32, i32, String, Tone)>,
    pub buttons: Vec<Rect>,
}

pub const PAD: i32 = 12;
pub const GAP: i32 = 8;

/// A card's width on a `sw`-pixel-wide screen.
pub fn card_width(sw: i32) -> i32 {
    (sw - 2 * PAD).clamp(200, 720)
}

/// The width text wraps to inside a card.
pub fn inner_width(sw: i32) -> i32 {
    card_width(sw) - 2 * PAD
}

/// A button's height with `line_h`-pixel lines.
pub fn button_height(line_h: i32) -> i32 {
    line_h + 2 * GAP
}

/// `text` cut to `width` pixels, ending in "..." when cut.
pub fn fit_text(text: &str, width: i32, measure: &dyn Fn(&str) -> i32) -> String {
    if measure(text) <= width {
        return text.to_owned();
    }
    let mut out: String = text.to_owned();
    while !out.is_empty() && measure(&format!("{out}...")) > width {
        out.pop();
    }
    format!("{}...", out.trim_end())
}

/// Wraps `text` at word boundaries to `width` pixels (`measure` gives a string's width); a word
/// wider than the line is cut by characters.
pub fn wrap(text: &str, width: i32, measure: &dyn Fn(&str) -> i32) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in text.split(' ') {
        let candidate = if cur.is_empty() { word.to_owned() } else { format!("{cur} {word}") };
        if measure(&candidate) <= width {
            cur = candidate;
            continue;
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        let mut piece = String::new();
        for ch in word.chars() {
            piece.push(ch);
            if measure(&piece) > width && piece.chars().count() > 1 {
                piece.pop();
                out.push(std::mem::take(&mut piece));
                piece.push(ch);
            }
        }
        cur = piece;
    }
    out.push(cur);
    out
}

/// Lays `card` out centred on a `sw`×`sh` screen, with `line_h`-pixel lines.
pub fn layout(card: &Card, sw: i32, sh: i32, line_h: i32, measure: &dyn Fn(&str) -> i32) -> Layout {
    layout_parts(&card.lines, &card.buttons, sw, sh, line_h, measure)
}

/// Lays lines and buttons out as a card: the lines wrapped, then the buttons in rows, left to
/// right (a wide button on a row of its own). Each line adds `line_h` and each row of buttons
/// [`button_height`] + [`GAP`] to the height, so a caller can tell what fits (the plugins panel's
/// pages).
pub fn layout_parts<A: Act>(lines: &[Line], buttons: &[Button<A>], sw: i32, sh: i32, line_h: i32, measure: &dyn Fn(&str) -> i32) -> Layout {
    let w = card_width(sw);
    let inner = w - 2 * PAD;
    let wrapped: Vec<(String, Tone)> =
        lines.iter().flat_map(|l| wrap(&l.text, inner, measure).into_iter().map(move |t| (t, l.tone))).collect();
    let button_h = button_height(line_h);
    let mut rows: Vec<Vec<(usize, i32)>> = Vec::new();
    let mut used = 0;
    for (i, b) in buttons.iter().enumerate() {
        let bw = if b.wide { inner } else { card_button_width(b, measure) };
        if used == 0 || b.wide || used + GAP + bw > inner {
            rows.push(Vec::new());
            used = 0;
        }
        used += if used > 0 { GAP } else { 0 } + bw;
        rows.last_mut().unwrap().push((i, bw));
        if b.wide {
            // Nothing beside it: the next button starts a row.
            used = inner + GAP;
        }
    }
    let h = 2 * PAD + wrapped.len() as i32 * line_h + GAP + rows.len() as i32 * (button_h + GAP);
    let panel = Rect { x: (sw - w) / 2, y: ((sh - h) / 2).max(0), w, h };
    let mut y = panel.y + PAD;
    let lines = wrapped
        .into_iter()
        .map(|(t, tone)| {
            let l = (panel.x + PAD, y, t, tone);
            y += line_h;
            l
        })
        .collect();
    y += GAP;
    let mut buttons = vec![Rect::default(); buttons.len()];
    for row in rows {
        let mut x = panel.x + PAD;
        for (i, bw) in row {
            buttons[i] = Rect { x, y, w: bw, h: button_h };
            x += bw + GAP;
        }
        y += button_h + GAP;
    }
    Layout { panel, lines, buttons }
}

/// A button is as wide as its longer label, so Shift doesn't move the others.
fn card_button_width<A: Act>(b: &Button<A>, measure: &dyn Fn(&str) -> i32) -> i32 {
    let label = measure(&b.label);
    let shifted = b.shift.map_or(0, |(l, _)| measure(l));
    label.max(shifted) + 2 * PAD + measure(" (2)")
}

/// The button under (x, y).
pub fn button_at(l: &Layout, x: i32, y: i32) -> Option<usize> {
    l.buttons.iter().position(|r| r.contains(x, y))
}

/// A press and a release (left button) on a card: the button pressed, to be released on the same
/// button. Approving buttons count only when `armed`.
#[derive(Clone, Debug, Default)]
pub struct Clicks {
    pressed: Option<usize>,
}

impl Clicks {
    /// The left button went down on `at`.
    pub fn press(&mut self, at: Option<usize>) {
        self.pressed = at;
    }
    /// The left button came up on `at`: the action, if it's a click on an enabled button.
    pub fn release(&mut self, card: &Card, at: Option<usize>, armed: bool, shift: bool) -> Option<Action> {
        self.release_in(&card.buttons, at, armed, shift)
    }
    /// [`Clicks::release`] over any card's buttons (the plugins panel's too).
    pub fn release_in<A: Act>(&mut self, buttons: &[Button<A>], at: Option<usize>, armed: bool, shift: bool) -> Option<A> {
        let pressed = self.pressed.take()?;
        if at != Some(pressed) {
            return None;
        }
        let (_, action) = buttons.get(pressed)?.current(shift);
        (!action.approves() || armed).then_some(action)
    }
    /// The card changed: a press on the old one doesn't carry over.
    pub fn reset(&mut self) {
        self.pressed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::offers::flow::{Checked, Update};
    use plugin_pkg::{Manifest, Sha256, Signer, TrustInfo, Version};

    fn measure(s: &str) -> i32 {
        8 * s.chars().count() as i32
    }

    fn info(update: Option<Update>, declares: &str) -> CardInfo {
        let m = format!(
            "format = 1\nid = \"web\"\nname = \"Web overlay\"\nversion = \"0.2.0\"\ndescription = \"A Chromium overlay.\"\nhomepage = \"https://example.org/web\"\nclient_ext_api = 1\n\n[platforms]\nlinux-x86_64 = \"lib/w.so\"\n{declares}\n[[files]]\npath = \"lib/w.so\"\nsize = 2048\nsha256 = \"{}\"\n[[files]]\npath = \"bin/host\"\nsize = 10\nsha256 = \"{}\"\n",
            Sha256::of_bytes(b"a"),
            Sha256::of_bytes(b"b")
        );
        let manifest = Manifest::parse(m.as_bytes()).unwrap();
        let trust = TrustInfo { trust: Trust::Unsigned, publisher: None, fingerprint: None, approved_before: None };
        CardInfo {
            checked: Checked { manifest, bytes: m.into_bytes(), sha256: Sha256::of_bytes(b"m"), signer: Signer::Unsigned, trust },
            update,
            installed: None,
        }
    }

    fn the_offer(flags: u8, note: &str) -> Offer {
        Offer { id: "web".into(), version: "0.2.0".into(), size: 3 << 20, sha256: [0; 32], flags, url: "u".into(), note: note.into() }
    }

    #[test]
    fn offer_card_contents() {
        let c = offer(
            &the_offer(1, "Needed for the vote page."),
            &info(None, "[declares]\nprocesses = [\"bin/host\"]\nnetwork = true\ndata = true\n"),
            Platform::LinuxX86_64,
            "Bob's TS (203.0.113.5:27015)",
        );
        assert_eq!(c.kind, Kind::Offer);
        let s = c.summary();
        for want in [
            "Bob's TS (203.0.113.5:27015) offers a client plugin",
            "Web overlay 0.2.0 (web)",
            "Unsigned: nobody vouches for this file",
            "https://example.org/web",
            "A Chromium overlay.",
            "Download 3.0 MB, installed 2 KB",
            "It says it starts programs: host, uses the network, stores data.",
            "Required by this server.",
            "The server says: Needed for the vote page.",
            NATIVE,
        ] {
            assert!(s.contains(want), "{want:?} in {s}");
        }
        assert_eq!(c.labels(), ["Install and allow", "Allow on this server only", "Not now", "Never from this server"]);
        assert_eq!(c.buttons[3].current(true), ("Never (everywhere)", Action::NeverEverywhere), "Shift");
        assert_eq!(c.default_action(), Action::Later);
    }

    #[test]
    fn update_card_contents() {
        let u = Update {
            old: Version::new(0, 3, 0),
            change: VersionChange::Downgrade,
            summary: Some(plugin_pkg::UpdateSummary {
                version: VersionChange::Downgrade,
                same_key: false,
                declares_changed: vec!["network"],
                files_changed: 2,
            }),
            key_changed: true,
        };
        let c = offer(&the_offer(0, ""), &info(Some(u), ""), Platform::LinuxX86_64, "203.0.113.5:27015");
        assert_eq!(c.kind, Kind::Update);
        let s = c.summary();
        assert!(s.contains("Update: Web overlay 0.3.0 -> 0.2.0"), "{s}");
        assert!(
            s.contains("OLDER version")
                && s.contains("publisher key changed")
                && s.contains("2 file(s) changed; declares changed: network")
        );
        assert!(!s.contains("Required"));
        assert_eq!(c.labels()[0], "Install update");
    }

    #[test]
    fn port_and_restart_cards() {
        let c = port("Bob's TS (203.0.113.5:27015)", ">=3.0.9", "tsmod.org", "3.0.8");
        assert!(c.summary().contains("It needs >=3.0.9; this game is 3.0.8.") && c.summary().contains("tsmod.org"));
        assert_eq!(c.buttons.iter().map(|b| b.action).collect::<Vec<_>>(), [Action::Update, Action::Later]);
        let r = restart("x", "Web overlay", "0.2.0", "web");
        assert_eq!(r.labels(), ["Restart now", "Later"]);
    }

    #[test]
    fn layout_and_clicks() {
        let c = offer(&the_offer(0, ""), &info(None, ""), Platform::LinuxX86_64, "server");
        let l = layout(&c, 1024, 768, 16, &measure);
        assert!(l.panel.w <= 720 && l.panel.x > 0 && l.panel.y >= 0 && l.panel.y + l.panel.h <= 768);
        for (x, _, t, _) in &l.lines {
            assert!(x + measure(t) <= l.panel.x + l.panel.w - PAD, "{t:?} fits");
        }
        for (i, b) in l.buttons.iter().enumerate() {
            assert!(b.x + b.w <= l.panel.x + l.panel.w - PAD && b.y + b.h <= l.panel.y + l.panel.h, "button {i} inside");
            for o in &l.buttons[i + 1..] {
                assert!(!(b.contains(o.x, o.y) || o.contains(b.x, b.y)), "buttons don't overlap");
            }
        }
        let small = layout(&c, 640, 480, 16, &measure);
        assert!(small.buttons.iter().map(|b| b.y).collect::<std::collections::BTreeSet<_>>().len() > 1, "rows on a small screen");
        let centre = |r: &Rect| (r.x + r.w / 2, r.y + r.h / 2);
        let (ix, iy) = centre(&l.buttons[0]);
        let mut k = Clicks::default();
        // Unarmed: the approving button does nothing; "Not now" works at once.
        k.press(button_at(&l, ix, iy));
        assert_eq!(k.release(&c, button_at(&l, ix, iy), false, false), None);
        let (nx, ny) = centre(&l.buttons[2]);
        k.press(button_at(&l, nx, ny));
        assert_eq!(k.release(&c, button_at(&l, nx, ny), false, false), Some(Action::Later));
        // Armed: press and release on the same button.
        k.press(button_at(&l, ix, iy));
        assert_eq!(k.release(&c, button_at(&l, nx, ny), true, false), None, "released elsewhere");
        k.press(button_at(&l, ix, iy));
        assert_eq!(k.release(&c, button_at(&l, ix, iy), true, false), Some(Action::Install));
        assert_eq!(k.release(&c, button_at(&l, ix, iy), true, false), None, "a release without a press");
        let (vx, vy) = centre(&l.buttons[3]);
        k.press(button_at(&l, vx, vy));
        assert_eq!(k.release(&c, button_at(&l, vx, vy), false, true), Some(Action::NeverEverywhere));
        k.press(button_at(&l, ix, iy));
        k.reset();
        assert_eq!(k.release(&c, button_at(&l, ix, iy), true, false), None, "a card change drops the press");
    }

    #[test]
    fn wide_buttons_take_a_row_each() {
        let buttons = vec![
            Button::wide("first row", Action::Later),
            Button::wide("second row", Action::Later),
            Button::new("A", Action::Later),
            Button::new("B", Action::Later),
        ];
        let lines = [line("title", Tone::Title)];
        let l = layout_parts(&lines, &buttons, 800, 600, 16, &measure);
        let inner = inner_width(800);
        assert_eq!((l.buttons[0].w, l.buttons[1].w), (inner, inner));
        assert!(l.buttons[1].y > l.buttons[0].y && l.buttons[2].y > l.buttons[1].y, "one row each, then the rest");
        assert_eq!(l.buttons[2].y, l.buttons[3].y, "small buttons share a row");
        let rows = 3;
        assert_eq!(l.panel.h, 2 * PAD + 16 + GAP + rows * (button_height(16) + GAP), "the height adds up per line and row");
        assert_eq!(layout_parts::<Action>(&lines, &[], 800, 600, 16, &measure).panel.h, 2 * PAD + 16 + GAP, "no buttons, no row");
    }

    #[test]
    fn fitting_text() {
        assert_eq!(fit_text("short", 80, &measure), "short");
        let cut = fit_text("a rather long label", 10 * 8, &measure);
        assert!(cut.ends_with("...") && measure(&cut) <= 80, "{cut}");
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("aaa bbb ccc", 7 * 8, &measure), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4 * 8, &measure), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 80, &measure), [""]);
        assert_eq!(size_text(3 << 20), "3.0 MB");
        assert_eq!(server_text("", "1.2.3.4:5"), "1.2.3.4:5");
        assert_eq!(server_text("Bob\n", "1.2.3.4:5"), "Bob (1.2.3.4:5)");
    }
}
