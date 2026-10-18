//! The plugins panel's pages and navigation, laid out as an offer card (`offers::card`): the same
//! lines, buttons, layout, click handling and drawing. Engine-free; the tests drive it with a
//! fixed-width font.
//!
//! Three views: the list (a wide button per plugin), one plugin's details (its approvals, what
//! start-up found, every file and its SHA-256, and the buttons that change it), and the
//! confirmation of a change, whose button is the only one that writes: click-only and armed
//! after [`crate::plugins::click::ARM_DELAY`], like an approving button on a card (§7.3). Long
//! lists and file lists are paged to what fits the screen ("Previous" / "Next", the mouse wheel,
//! Page Up / Page Down).

use super::super::offers::card::{self, Act, Button, Layout, Line, Tone, line};
use super::model::{Op, Row};

/// What a panel button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// A row of the list: its details.
    Open(usize),
    Previous,
    Next,
    /// From the details to the list, or from a confirmation to the details.
    Back,
    Close,
    /// A change's button in the details: its confirmation.
    Ask(Op),
    /// The confirmation's button: does it.
    Confirm,
}

impl Act for Action {
    fn approves(self) -> bool {
        self == Action::Confirm
    }
}

/// A page's first item, and the first items of the pages before it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paging {
    pub start: usize,
    back: Vec<usize>,
}

impl Paging {
    fn next(&mut self, to: usize) {
        if to > self.start {
            self.back.push(std::mem::replace(&mut self.start, to));
        }
    }
    fn previous(&mut self) {
        if let Some(s) = self.back.pop() {
            self.start = s;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    List(Paging),
    Detail { row: usize, page: Paging },
    Confirm { row: usize, op: Op },
}

/// The panel: its rows, the view, and the last change's result.
#[derive(Clone, Debug)]
pub struct Panel {
    pub rows: Vec<Row>,
    pub view: View,
    /// The last change's result (and whether it failed), shown at the top of the list.
    pub message: Option<(String, bool)>,
    /// Shown when nothing can be changed (the store couldn't be read at start, or `cl_plugins 0`).
    pub note: Option<String>,
}

/// A screen and the font: what the pages are cut to.
pub struct Screen<'a> {
    pub width: i32,
    pub height: i32,
    pub line_h: i32,
    pub measure: &'a dyn Fn(&str) -> i32,
}

/// Room kept free above and below the panel.
const MARGIN: i32 = 8;

/// A view, built for one screen: what to draw and hit-test, and where the next page starts.
#[derive(Clone, Debug)]
pub struct Built {
    pub lines: Vec<Line>,
    pub buttons: Vec<Button<Action>>,
    pub layout: Layout,
    /// The first item of the next page, when there is one.
    pub next: Option<usize>,
}

/// What a click asks the caller to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Nothing more (navigation happened in the panel).
    None,
    Close,
    /// Do `op` to `rows[row]`, then [`Panel::done`].
    Apply {
        row: usize,
        op: Op,
    },
}

impl Panel {
    pub fn new(rows: Vec<Row>) -> Panel {
        Panel { rows, view: View::List(Paging::default()), message: None, note: None }
    }

    /// A change was done (or failed): back to the list, with the result and the new rows.
    pub fn done(&mut self, rows: Vec<Row>, message: String, failed: bool) {
        self.rows = rows;
        self.view = View::List(Paging::default());
        self.message = Some((message, failed));
    }

    /// Acts on a clicked button (or a paging key). `next` is the built view's.
    pub fn act(&mut self, a: Action, next: Option<usize>) -> Effect {
        match (&mut self.view, a) {
            (_, Action::Close) => return Effect::Close,
            (View::List(p) | View::Detail { page: p, .. }, Action::Next) => {
                if let Some(n) = next {
                    p.next(n);
                }
            }
            (View::List(p) | View::Detail { page: p, .. }, Action::Previous) => p.previous(),
            (View::List(_), Action::Open(row)) if row < self.rows.len() => {
                self.view = View::Detail { row, page: Paging::default() };
            }
            (View::Detail { .. }, Action::Back) => self.view = View::List(Paging::default()),
            (View::Detail { row, .. }, Action::Ask(op)) if self.note.is_none() && op.offered(&self.rows[*row]) => {
                self.view = View::Confirm { row: *row, op };
            }
            (View::Confirm { row, .. }, Action::Back) => self.view = View::Detail { row: *row, page: Paging::default() },
            (View::Confirm { row, op }, Action::Confirm) => return Effect::Apply { row: *row, op: *op },
            _ => {}
        }
        Effect::None
    }

    /// The view on `s`, cut to what fits.
    pub fn build(&self, s: &Screen) -> Built {
        match &self.view {
            View::List(page) => self.build_list(page, s),
            View::Detail { row, page } => self.build_detail(*row, page, s),
            View::Confirm { row, op } => self.build_confirm(*row, *op, s),
        }
    }

    fn build_list(&self, page: &Paging, s: &Screen) -> Built {
        let mut head = vec![line("Client plugins", Tone::Title)];
        if let Some((m, failed)) = &self.message {
            head.push(line(m.clone(), if *failed { Tone::Warn } else { Tone::Normal }));
        }
        if let Some(n) = &self.note {
            head.push(line(n.clone(), Tone::Warn));
        }
        if self.rows.is_empty() {
            head.push(line("No client plugins are installed or approved.", Tone::Dim));
        } else {
            head.push(line("Click one to see its files and hashes, or to revoke, unblock or delete it.", Tone::Dim));
        }
        // The count line's room is kept whatever it says.
        let count = |a: usize, b: usize| line(format!("Plugins {a}-{b} of {}", self.rows.len()), Tone::Dim);
        let tail = vec![Button::new("Close", Action::Close)];
        let item_h = card::button_height(s.line_h) + card::GAP;
        let items: Vec<i32> = vec![item_h; self.rows.len()];
        let (n, paged) = fit(s, &[head.clone(), vec![count(9999, 9999)]].concat(), &tail, &items, page.start);
        let mut lines = head;
        if paged {
            lines.push(count(page.start + 1, page.start + n));
        }
        let mut buttons: Vec<Button<Action>> =
            (page.start..page.start + n).map(|i| Button::wide(self.rows[i].summary(), Action::Open(i))).collect();
        nav(&mut buttons, paged, page, page.start + n < self.rows.len());
        buttons.extend(tail);
        finish(lines, buttons, s, (page.start + n < self.rows.len()).then_some(page.start + n))
    }

    fn build_detail(&self, i: usize, page: &Paging, s: &Screen) -> Built {
        let row = &self.rows[i];
        let mut head = vec![line(format!("{} ({})", row.title(), row.id), Tone::Title)];
        if row.entries.is_empty() {
            head.push(line("Approval: none", Tone::Normal));
        }
        for e in &row.entries {
            let version = if row.version.as_deref() == Some(&e.version.to_string()) || e.is_manual() {
                String::new()
            } else {
                format!(" version {}", e.version)
            };
            let tone = if e.state == plugin_pkg::State::Blocked { Tone::Warn } else { Tone::Normal };
            head.push(line(format!("Approval: {}{version}, {} at {}", super::super::entry_text(e), e.platform, e.approved_at), tone));
        }
        if row.installed {
            head.push(line(format!("At start: {}", row.status), Tone::Normal));
            let place = row.dir.as_ref().map_or_else(|| "cl_dlls/".to_string(), |d| super::model::shown_dir(d));
            head.push(line(format!("Installed in {place}"), Tone::Dim));
        } else {
            head.push(line("Not installed.", Tone::Dim));
        }
        if row.loaded && row.dir.is_some() {
            head.push(line("Loaded now: it can be deleted once a start doesn't load it.", Tone::Dim));
        }
        let what = if row.installed { "found at start" } else { "approved" };
        let count = |a: usize, b: usize| line(format!("{} file(s) {what}, with SHA-256 (files {a}-{b}):", row.files.len()), Tone::Normal);
        let mut tail: Vec<Button<Action>> = Vec::new();
        if self.note.is_none() {
            for op in [Op::Revoke, Op::Unblock, Op::Delete] {
                if op.offered(row) {
                    tail.push(Button::new(op.label(), Action::Ask(op)));
                }
            }
        }
        tail.push(Button::new("Back", Action::Back));
        tail.push(Button::new("Close", Action::Close));
        let inner = card::inner_width(s.width);
        let file_lines = |f: &super::model::FileRow| {
            vec![line(format!("{}{}", f.path, f.mark.text()), file_tone(f.mark)), line(format!("  {}", f.hash), Tone::Dim)]
        };
        let items: Vec<i32> = row
            .files
            .iter()
            .map(|f| file_lines(f).iter().map(|l| card::wrap(&l.text, inner, s.measure).len() as i32).sum::<i32>() * s.line_h)
            .collect();
        let (n, paged) = fit(s, &[head.clone(), vec![count(9999, 9999)]].concat(), &tail, &items, page.start);
        let mut lines = head;
        if !row.files.is_empty() {
            lines.push(count(page.start + 1, page.start + n));
        }
        lines.extend(row.files[page.start..page.start + n].iter().flat_map(file_lines));
        let mut buttons = Vec::new();
        nav(&mut buttons, paged, page, page.start + n < row.files.len());
        buttons.extend(tail);
        finish(lines, buttons, s, (page.start + n < row.files.len()).then_some(page.start + n))
    }

    fn build_confirm(&self, i: usize, op: Op, s: &Screen) -> Built {
        let row = &self.rows[i];
        let mut lines: Vec<Line> = op.confirm_lines(row).into_iter().map(|t| line(t, Tone::Normal)).collect();
        lines[0].tone = Tone::Title;
        let buttons = vec![Button::new(op.label(), Action::Confirm), Button::new("Cancel", Action::Back)];
        finish(lines, buttons, s, None)
    }
}

fn file_tone(m: super::model::Mark) -> Tone {
    use super::model::Mark;
    match m {
        Mark::Plain | Mark::Matches => Tone::Normal,
        Mark::Differs | Mark::Extra | Mark::Missing => Tone::Warn,
    }
}

/// "Previous" and "Next" when the items take more than one page.
fn nav(buttons: &mut Vec<Button<Action>>, paged: bool, page: &Paging, more: bool) {
    if !paged {
        return;
    }
    if page.start > 0 {
        buttons.push(Button::new("< Previous", Action::Previous));
    }
    if more {
        buttons.push(Button::new("Next >", Action::Next));
    }
}

fn finish(lines: Vec<Line>, buttons: Vec<Button<Action>>, s: &Screen, next: Option<usize>) -> Built {
    let layout = card::layout_parts(&lines, &buttons, s.width, s.height, s.line_h, s.measure);
    Built { lines, buttons, layout, next }
}

/// How many items (of heights `items`, from `start`) fit beside `head` and `tail` on the
/// screen, at least one; and whether they need paging (the "Previous"/"Next" row is counted
/// then).
fn fit(s: &Screen, head: &[Line], tail: &[Button<Action>], items: &[i32], start: usize) -> (usize, bool) {
    let room = s.height - 2 * MARGIN;
    let height = |buttons: &[Button<Action>]| card::layout_parts(head, buttons, s.width, s.height, s.line_h, s.measure).panel.h;
    let count = |base: i32| {
        let mut used = base;
        let mut n = 0;
        for h in &items[start.min(items.len())..] {
            if n > 0 && used + h > room {
                break;
            }
            used += h;
            n += 1;
        }
        n
    };
    let all = items.len().saturating_sub(start);
    let n = count(height(tail));
    if start == 0 && n == all {
        return (n, false);
    }
    // Paged: room for the navigation buttons, which share a row with the tail's when they fit.
    let nav: Vec<Button<Action>> = [Button::new("< Previous", Action::Previous), Button::new("Next >", Action::Next)]
        .into_iter()
        .chain(tail.iter().cloned())
        .collect();
    (count(height(&nav)).min(all), true)
}

#[cfg(test)]
mod tests {
    use super::super::model::{FileRow, Mark};
    use super::*;
    use plugin_pkg::Sha256;
    use std::path::PathBuf;

    fn measure(s: &str) -> i32 {
        8 * s.chars().count() as i32
    }

    fn screen(width: i32, height: i32) -> Screen<'static> {
        Screen { width, height, line_h: 16, measure: &measure }
    }

    fn row(id: &str, files: usize) -> Row {
        Row {
            id: id.into(),
            name: id.into(),
            version: Some("1.0.0".into()),
            entries: vec![],
            dir: Some(PathBuf::from(format!("/g/cl_plugins/{id}/1.0.0"))),
            manual: false,
            installed: true,
            loaded: false,
            status: "not loaded".into(),
            files: (0..files)
                .map(|i| FileRow { path: format!("files/f{i}"), hash: Sha256::of_bytes(&[i as u8]), mark: Mark::Plain })
                .collect(),
        }
    }

    fn centre(b: &Built, i: usize) -> (i32, i32) {
        let r = b.layout.buttons[i];
        (r.x + r.w / 2, r.y + r.h / 2)
    }

    fn fits(b: &Built, s: &Screen) {
        let p = b.layout.panel;
        assert!(p.y >= 0 && p.y + p.h <= s.height, "the panel fits: {p:?} on {}x{}", s.width, s.height);
        for (x, _, t, _) in &b.layout.lines {
            assert!(x + measure(t) <= p.x + p.w - card::PAD, "{t:?} fits");
        }
    }

    /// Clicks the button whose action is `a`, through the layout and the click rules.
    fn click(p: &mut Panel, s: &Screen, a: Action, armed: bool) -> Option<Effect> {
        let b = p.build(s);
        let i = b.buttons.iter().position(|x| x.action == a)?;
        let (x, y) = centre(&b, i);
        let mut k = card::Clicks::default();
        k.press(card::button_at(&b.layout, x, y));
        let got = k.release_in(&b.buttons, card::button_at(&b.layout, x, y), armed, false)?;
        Some(p.act(got, b.next))
    }

    #[test]
    fn many_plugins_page_on_a_small_screen() {
        let mut p = Panel::new((0..40).map(|i| row(&format!("p{i:02}"), 1)).collect());
        let s = screen(640, 480);
        let b = p.build(&s);
        fits(&b, &s);
        let shown = b.buttons.iter().filter(|x| matches!(x.action, Action::Open(_))).count();
        assert!(shown > 3 && shown < 40, "{shown} rows on a page");
        assert_eq!(b.next, Some(shown));
        assert!(b.lines.iter().any(|l| l.text == format!("Plugins 1-{shown} of 40")));
        assert!(!b.buttons.iter().any(|x| x.action == Action::Previous) && b.buttons.iter().any(|x| x.action == Action::Next));
        // Page through to the end and back.
        let mut seen = shown;
        while click(&mut p, &s, Action::Next, false).is_some() {
            let b = p.build(&s);
            fits(&b, &s);
            seen += b.buttons.iter().filter(|x| matches!(x.action, Action::Open(_))).count();
        }
        assert_eq!(seen, 40, "every row on some page");
        assert!(p.build(&s).buttons.iter().any(|x| x.action == Action::Previous));
        while click(&mut p, &s, Action::Previous, false).is_some() {}
        assert_eq!(p.view, View::List(Paging::default()));
        // Few plugins: no paging.
        let b = Panel::new(vec![row("a", 1), row("b", 1)]).build(&screen(1024, 768));
        assert!(b.next.is_none() && !b.buttons.iter().any(|x| matches!(x.action, Action::Next | Action::Previous)));
        assert!(!b.lines.iter().any(|l| l.text.starts_with("Plugins ")));
    }

    #[test]
    fn details_show_every_file_and_hash_across_pages() {
        let mut p = Panel::new(vec![row("a", 1), row("web", 150)]);
        let s = screen(640, 480);
        assert_eq!(click(&mut p, &s, Action::Open(1), false), Some(Effect::None));
        assert!(matches!(p.view, View::Detail { row: 1, .. }));
        let mut hashes = Vec::new();
        loop {
            let b = p.build(&s);
            fits(&b, &s);
            hashes.extend(b.lines.iter().filter_map(|l| l.text.strip_prefix("  ").map(str::to_owned)));
            if click(&mut p, &s, Action::Next, false).is_none() {
                break;
            }
        }
        let want: Vec<String> = p.rows[1].files.iter().map(|f| f.hash.to_string()).collect();
        assert_eq!(hashes, want, "every hash, in order, once");
        assert_eq!(click(&mut p, &s, Action::Back, false), Some(Effect::None));
        assert_eq!(p.view, View::List(Paging::default()));
    }

    #[test]
    fn a_change_needs_its_confirmation_armed() {
        let mut r = row("web", 2);
        r.entries = vec![];
        let mut p = Panel::new(vec![r]);
        let s = screen(800, 600);
        click(&mut p, &s, Action::Open(0), false);
        let labels: Vec<String> = p.build(&s).buttons.iter().map(|b| b.label.to_string()).collect();
        assert_eq!(labels, ["Delete files", "Back", "Close"], "only what applies");
        assert_eq!(click(&mut p, &s, Action::Ask(Op::Revoke), false), None, "no revoke without an approval");
        assert_eq!(p.act(Action::Ask(Op::Revoke), None), Effect::None);
        assert!(matches!(p.view, View::Detail { .. }), "a revoke it doesn't offer does nothing");
        assert_eq!(click(&mut p, &s, Action::Ask(Op::Delete), false), Some(Effect::None), "asking needs no delay");
        assert_eq!(p.view, View::Confirm { row: 0, op: Op::Delete });
        let b = p.build(&s);
        assert!(b.lines[0].text.starts_with("Delete web 1.0.0 (web)?"));
        assert_eq!(click(&mut p, &s, Action::Confirm, false), None, "unarmed: the click doesn't count");
        assert_eq!(p.view, View::Confirm { row: 0, op: Op::Delete });
        assert_eq!(click(&mut p, &s, Action::Confirm, true), Some(Effect::Apply { row: 0, op: Op::Delete }));
        p.done(vec![], "deleted web".into(), false);
        assert_eq!(p.view, View::List(Paging::default()));
        assert!(p.build(&s).lines.iter().any(|l| l.text == "deleted web" && l.tone == Tone::Normal));
        assert_eq!(p.act(Action::Close, None), Effect::Close);
    }

    #[test]
    fn cancel_and_a_note_change_nothing() {
        let mut r = row("web", 1);
        r.loaded = true;
        let mut p = Panel::new(vec![r]);
        let s = screen(800, 600);
        click(&mut p, &s, Action::Open(0), false);
        assert!(!p.build(&s).buttons.iter().any(|b| b.action == Action::Ask(Op::Delete)), "no delete while loaded");
        p.rows[0].loaded = false;
        click(&mut p, &s, Action::Ask(Op::Delete), false);
        assert_eq!(click(&mut p, &s, Action::Back, false), Some(Effect::None), "Cancel");
        assert!(matches!(p.view, View::Detail { row: 0, .. }));
        p.note = Some("the store couldn't be read".into());
        assert!(!p.build(&s).buttons.iter().any(|b| matches!(b.action, Action::Ask(_))), "nothing to change");
        assert_eq!(p.act(Action::Ask(Op::Delete), None), Effect::None);
        assert!(matches!(p.view, View::Detail { .. }));
    }

    #[test]
    fn a_tiny_screen_still_shows_one_item() {
        let p = Panel::new(vec![row("web", 3)]);
        let s = screen(320, 120);
        let b = p.build(&s);
        assert_eq!(b.buttons.iter().filter(|x| matches!(x.action, Action::Open(_))).count(), 1);
        assert!(b.layout.panel.w <= 320);
    }
}
