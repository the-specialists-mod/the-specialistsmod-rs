//! The restart and reconnect cards (docs/implementation/client-plugins.md §13.5): quit and
//! inform, "Reconnect to <server>?" at the next start, and `tsrs_reconnect`'s password card. Each
//! is a small modal window of its own ([`goldsrc::platform::sdl::Window`]), because the main menu
//! has no HUD to draw on. They follow §7.3: the approving button takes only a mouse click (never
//! Enter or a bound key) and stays disabled for [`CLICK_DELAY_MS`]; Escape and closing the window
//! are the other button. A password field shows `*` for each character.
//!
//! This module holds the layout, the drawing (filled rectangles, the font8x8 glyphs) and the
//! event handling, which the tests drive; [`show`] runs it in the window.

use super::plan::{Reason, Request};
use super::reconnect::Reconnect;
use goldsrc::platform::sdl::{Event, Key};

/// The approving button is disabled for this long after the card appears: the delay every
/// approval shares (`crate::plugins::click::ARM_DELAY`, client-plugins.md §7.3).
pub const CLICK_DELAY_MS: u64 = crate::plugins::click::ARM_DELAY.as_millis() as u64;
/// The longest password the field takes (a cvar string the engines keep whole).
pub const MAX_PASSWORD: usize = 63;

const SCALE: i32 = 2;
const CHAR: i32 = 8 * SCALE;
const LINE: i32 = 20;
const PAD: i32 = 16;
const COLS: usize = 44;
const BUTTON_H: i32 = 32;
const FIELD_H: i32 = 28;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Case 4: "Quit now" / "Stay".
    Inform,
    /// The next start's "Reconnect to <server>?".
    Reconnect,
    /// `tsrs_reconnect <address> password`.
    Password,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Inform => "inform",
            Kind::Reconnect => "reconnect",
            Kind::Password => "password",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub kind: Kind,
    pub title: String,
    pub lines: Vec<String>,
    /// A masked password field.
    pub password: bool,
    /// The approving button (click only, after the delay).
    pub ok: String,
    /// The default button: Escape and closing the window choose it.
    pub cancel: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// The approving button, with the typed password when the card has a field.
    Ok(Option<String>),
    Cancel,
}

const CLIPBOARD_LINE: &str = "The address is on the clipboard.";

impl Card {
    /// Case 4: nothing else can restart the game.
    pub fn inform(req: &Request, clipboard: bool) -> Card {
        let mut lines = vec![format!("Restart the game and reconnect to {}.", req.server_label())];
        if let Reason::Port { requirement, url } = &req.reason {
            let url = if url.is_empty() { "tsmod.org" } else { url };
            lines.push(format!("This server needs version {requirement}; get it from {url} or the TS launcher."));
        }
        if clipboard {
            lines.push(CLIPBOARD_LINE.into());
        }
        lines.push("At the next start the game offers to reconnect.".into());
        Card { kind: Kind::Inform, title: "Restart needed".into(), lines, password: false, ok: "Quit now".into(), cancel: "Stay".into() }
    }

    /// The next start, from `.reconnect`.
    pub fn reconnect(r: &Reconnect) -> Card {
        let mut lines = vec![format!("Reconnect to {}?", r.label())];
        if r.password_required {
            lines.push("This server needs a password:".into());
        }
        Card {
            kind: Kind::Reconnect,
            title: "Reconnect".into(),
            lines,
            password: r.password_required,
            ok: "Reconnect".into(),
            cancel: "Cancel".into(),
        }
    }

    /// `tsrs_reconnect <address> password`.
    pub fn password(address: &str) -> Card {
        Card {
            kind: Kind::Password,
            title: "Server password".into(),
            lines: vec![format!("The server at {address} needs a password:")],
            password: true,
            ok: "Connect".into(),
            cancel: "Cancel".into(),
        }
    }

    /// The card as one line, for the console and the e2e record.
    pub fn summary(&self) -> String {
        format!("{}: {}", self.title, self.lines.join(" "))
    }
}

/// Wraps `text` at word boundaries to `cols` characters (longer words are split).
fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > cols {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            out.push(word.drain(..cols).collect());
        }
        let word: String = word.into_iter().collect();
        if line.chars().count() + word.chars().count() + usize::from(!line.is_empty()) > cols {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

type Rect = [i32; 4];
/// A filled rectangle and its colour.
pub type Fill = (Rect, [u8; 3]);

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r[0] && y >= r[1] && x < r[0] + r[2] && y < r[1] + r[3]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    /// (y, text) of every text line under the title.
    pub lines: Vec<(i32, String)>,
    pub field: Option<Rect>,
    pub ok: Rect,
    pub cancel: Rect,
}

pub fn layout(card: &Card) -> Layout {
    let w = COLS as i32 * CHAR + 2 * PAD;
    let mut y = PAD + LINE + 8;
    let mut lines = Vec::new();
    for l in &card.lines {
        for part in wrap(l, COLS) {
            lines.push((y, part));
            y += LINE;
        }
    }
    let field = card.password.then(|| {
        let r = [PAD, y + 4, w - 2 * PAD, FIELD_H];
        y += FIELD_H + 8;
        r
    });
    y += 12;
    let bw = |label: &str| label.chars().count() as i32 * CHAR + 24;
    let cancel = [w - PAD - bw(&card.cancel), y, bw(&card.cancel), BUTTON_H];
    let ok = [cancel[0] - 12 - bw(&card.ok), y, bw(&card.ok), BUTTON_H];
    Layout { w, h: y + BUTTON_H + PAD, lines, field, ok, cancel }
}

/// A card being shown: what has been typed, and the pointer.
pub struct State {
    pub card: Card,
    pub layout: Layout,
    pub typed: String,
    opened_ms: u64,
    hover: Option<bool>,
    /// A short note under the buttons ("Click Reconnect", "Type the password").
    pub note: Option<&'static str>,
}

const BG: [u8; 3] = [24, 24, 24];
const BORDER: [u8; 3] = [200, 150, 40];
const TEXT: [u8; 3] = [230, 230, 230];
const TITLE: [u8; 3] = [255, 190, 60];
const DIM: [u8; 3] = [120, 120, 120];
const BUTTON: [u8; 3] = [64, 64, 64];
const HOVER: [u8; 3] = [96, 96, 96];
const FIELD: [u8; 3] = [0, 0, 0];

impl State {
    pub fn new(card: Card, now_ms: u64) -> State {
        let layout = layout(&card);
        State { card, layout, typed: String::new(), opened_ms: now_ms, hover: None, note: None }
    }

    pub fn ok_enabled(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.opened_ms) >= CLICK_DELAY_MS
    }

    /// Handles one event; `Some` when the card is answered.
    pub fn event(&mut self, ev: &Event, now_ms: u64) -> Option<Answer> {
        match ev {
            Event::Close | Event::Key(Key::Escape) => return Some(Answer::Cancel),
            // §7.3: the approving button takes only a click.
            Event::Key(Key::Return) => self.note = Some("Click a button."),
            Event::Key(Key::Backspace) => {
                self.typed.pop();
            }
            Event::Key(Key::Other) | Event::Redraw => {}
            Event::Text(t) if self.card.password => {
                for c in t.chars() {
                    if self.typed.len() >= MAX_PASSWORD {
                        self.note = Some("That's as long as a password can be.");
                    } else if (c.is_ascii_graphic() && c != '"') || c == ' ' {
                        self.typed.push(c);
                    } else {
                        self.note = Some("The password can't contain that character.");
                    }
                }
            }
            Event::Text(_) => {}
            Event::Motion(x, y) => {
                self.hover = if inside(self.layout.ok, *x, *y) {
                    Some(true)
                } else if inside(self.layout.cancel, *x, *y) {
                    Some(false)
                } else {
                    None
                };
            }
            Event::Click(x, y) => {
                if inside(self.layout.cancel, *x, *y) {
                    return Some(Answer::Cancel);
                }
                if inside(self.layout.ok, *x, *y) && self.ok_enabled(now_ms) {
                    if !self.card.password {
                        return Some(Answer::Ok(None));
                    }
                    if self.typed.is_empty() {
                        self.note = Some("Type the password.");
                    } else {
                        return Some(Answer::Ok(Some(self.typed.clone())));
                    }
                }
            }
        }
        None
    }

    /// What to draw, back to front.
    pub fn fills(&self, now_ms: u64) -> Vec<Fill> {
        let l = &self.layout;
        let mut f: Vec<Fill> = vec![([0, 0, l.w, l.h], BORDER), ([2, 2, l.w - 4, l.h - 4], BG)];
        text(&mut f, PAD, PAD, &self.card.title, TITLE);
        for (y, s) in &l.lines {
            text(&mut f, PAD, *y, s, TEXT);
        }
        if let Some(r) = l.field {
            f.push((r, DIM));
            f.push(([r[0] + 1, r[1] + 1, r[2] - 2, r[3] - 2], FIELD));
            let shown: String = "*".repeat(self.typed.chars().count()) + "_";
            text(&mut f, r[0] + 6, r[1] + (FIELD_H - CHAR) / 2, &shown, TEXT);
        }
        let enabled = self.ok_enabled(now_ms);
        for (r, label, is_ok) in [(l.ok, &self.card.ok, true), (l.cancel, &self.card.cancel, false)] {
            f.push((r, if self.hover == Some(is_ok) && (enabled || !is_ok) { HOVER } else { BUTTON }));
            let colour = if is_ok && !enabled { DIM } else { TEXT };
            text(&mut f, r[0] + 12, r[1] + (BUTTON_H - CHAR) / 2, label, colour);
        }
        if let Some(n) = self.note {
            text(&mut f, PAD, l.ok[1] + (BUTTON_H - CHAR) / 2, n, DIM);
        }
        f
    }
}

/// The font8x8 glyphs of `s` at (x, y), one rectangle per run of lit pixels in a glyph row.
fn text(f: &mut Vec<Fill>, x: i32, y: i32, s: &str, colour: [u8; 3]) {
    for (i, c) in s.chars().enumerate() {
        let glyph = font8x8::legacy::BASIC_LEGACY[if c.is_ascii() { c as usize } else { b'?' as usize }];
        let gx = x + i as i32 * CHAR;
        for (row, bits) in glyph.iter().enumerate() {
            let mut col = 0;
            while col < 8 {
                if bits & (1 << col) == 0 {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < 8 && bits & (1 << col) != 0 {
                    col += 1;
                }
                f.push(([gx + start * SCALE, y + row as i32 * SCALE, (col - start) * SCALE, SCALE], colour));
            }
        }
    }
}

/// Shows `card` until it is answered. Without SDL2 (or a window), the card is printed to the
/// console and answered `Cancel`, so nothing happens that the player didn't choose.
pub fn show(card: &Card) -> Answer {
    let e = goldsrc::client::engine();
    e.print(&format!("[ts] relaunch: card: {}\n", card.summary()));
    #[cfg(feature = "e2e")]
    if let Some(a) = super::e2e::answer(card) {
        return a;
    }
    let mut st = State::new(card.clone(), 0);
    let title = goldsrc::util::to_cstring(&card.title);
    let Some(win) = goldsrc::platform::sdl::Window::open(&title, st.layout.w, st.layout.h, card.password) else {
        e.print("[ts] relaunch: can't open a window for the card; nothing done\n");
        return Answer::Cancel;
    };
    let start = std::time::Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut enabled = false;
    win.draw(&st.fills(0));
    loop {
        let ev = win.wait(100);
        let t = now();
        match ev {
            Some(ev) => {
                if let Some(a) = st.event(&ev, t) {
                    return a;
                }
                win.draw(&st.fills(t));
            }
            None if st.ok_enabled(t) != enabled => {
                enabled = !enabled;
                win.draw(&st.fills(t));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn centre(r: Rect) -> (i32, i32) {
        (r[0] + r[2] / 2, r[1] + r[3] / 2)
    }

    #[test]
    fn clicks_only_after_the_delay() {
        let req = Request::new("203.0.113.5:27015", "Bob's TS", false, Reason::Restart).unwrap();
        let mut s = State::new(Card::inform(&req, true), 0);
        let (x, y) = centre(s.layout.ok);
        assert_eq!(s.event(&Event::Click(x, y), 500), None, "disabled for the first 2 s");
        assert_eq!(s.event(&Event::Key(Key::Return), 5000), None, "Enter never approves");
        assert_eq!(s.event(&Event::Click(x, y), 2000), Some(Answer::Ok(None)));
        let mut s = State::new(Card::inform(&req, false), 0);
        assert_eq!(s.event(&Event::Key(Key::Escape), 10), Some(Answer::Cancel));
        let mut s = State::new(Card::inform(&req, false), 0);
        let (x, y) = centre(s.layout.cancel);
        assert_eq!(s.event(&Event::Click(x, y), 10), Some(Answer::Cancel), "the other button works at once");
        let mut s = State::new(Card::inform(&req, false), 0);
        assert_eq!(s.event(&Event::Close, 10), Some(Answer::Cancel));
    }

    #[test]
    fn password_field() {
        let mut s = State::new(Card::password("203.0.113.5:27015"), 0);
        let (x, y) = centre(s.layout.ok);
        assert_eq!(s.event(&Event::Click(x, y), 3000), None, "an empty password isn't sent");
        s.event(&Event::Text("hunter2\"é".into()), 3000);
        assert_eq!(s.typed, "hunter2", "quotes and non-ASCII are refused");
        s.event(&Event::Key(Key::Backspace), 3000);
        s.event(&Event::Text("3; x".into()), 3000);
        assert_eq!(s.typed, "hunter3; x");
        // Drawn masked: no glyph of the password, only '*'s.
        let fills = s.fills(3000);
        let star_fills = {
            let mut f = Vec::new();
            text(&mut f, 0, 0, "*", TEXT);
            f.len()
        };
        assert!(fills.len() > star_fills * s.typed.len());
        assert_eq!(s.event(&Event::Click(x, y), 3000), Some(Answer::Ok(Some("hunter3; x".into()))));
        let mut s = State::new(Card::password("a:1"), 0);
        s.event(&Event::Text("x".repeat(100)), 0);
        assert_eq!(s.typed.len(), MAX_PASSWORD);
    }

    #[test]
    fn cards_say_what_happens() {
        let req = Request::new("203.0.113.5:27015", "Bob's TS", true, Reason::Port { requirement: ">=3.0.8".into(), url: String::new() })
            .unwrap();
        let c = Card::inform(&req, true);
        let all = c.lines.join("\n");
        assert!(all.contains("Restart the game and reconnect to Bob's TS (203.0.113.5:27015)"), "{all}");
        assert!(all.contains("needs version >=3.0.8; get it from tsmod.org or the TS launcher"), "{all}");
        assert!(all.contains(CLIPBOARD_LINE));
        let r = Reconnect::new("203.0.113.5:27015", "Bob's TS", true, 0);
        let c = Card::reconnect(&r);
        assert!(c.password && c.lines[0] == "Reconnect to Bob's TS (203.0.113.5:27015)?");
        assert!(!Card::reconnect(&Reconnect::new("1.2.3.4:1", "", false, 0)).password);
    }

    #[test]
    fn layout_fits() {
        assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        let req = Request::new(
            "203.0.113.5:27015",
            &"N".repeat(64),
            false,
            Reason::Port { requirement: ">=3.0.8, <3.1".into(), url: "u".repeat(96) },
        )
        .unwrap();
        let l = layout(&Card::inform(&req, true));
        for (_, s) in &l.lines {
            assert!(s.chars().count() <= COLS);
        }
        assert!(l.ok[0] > PAD && l.ok[0] + l.ok[2] < l.cancel[0] && l.cancel[0] + l.cancel[2] <= l.w - PAD);
        assert!(l.h < 600 && l.w < 800);
    }
}
