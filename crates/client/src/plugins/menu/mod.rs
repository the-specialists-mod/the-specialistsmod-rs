//! The plugins panel, `cl_plugins_menu` (port addition, issue #311; client-plugins.md §8, §9):
//! every plugin start-up found and every id the store knows, each with its approvals, what
//! start-up decided, and every file with its SHA-256; from it the player revokes an approval,
//! unblocks a "Never" and deletes an installed version.
//!
//! It's drawn and clicked exactly as the offer cards are (`super::offers::card`): in `HUD_Redraw`,
//! last, with the virtual cursor, its clicks only through `IN_MouseEvent` (left press and release
//! on the same button), Escape closing it. Not the SDL card windows, which stop the frames. While
//! it's open the extensions get no `redraw` and queued offer cards wait; a card already open
//! stays on top and takes the input first.
//!
//! **The store and the disk change only on a click** (§7.3): each change goes through a
//! confirmation whose button is armed after [`super::click::ARM_DELAY`]. `cl_plugins_menu` takes
//! no arguments and only opens the panel, so a server stuffing it can at worst show the panel.
//! A revoke takes effect at the next start for a loaded plugin (native code isn't unloaded, §9);
//! one that hasn't loaded yet (a `scope = "server"` approval waiting for its server) won't load.

pub mod model;
pub mod view;

#[cfg(feature = "e2e")]
mod e2e;

use super::click::Arming;
use super::offers::card::{self, Clicks};
use super::startup::Decision;
use goldsrc::Global;
use goldsrc::client::engine;
use model::{Op, Row};
use std::cell::RefCell;
use view::{Action, Effect, Panel, Screen};

struct Open {
    panel: Panel,
    arming: Arming,
    clicks: Clicks,
}

static MENU: Global<RefCell<Option<Open>>> = Global(RefCell::new(None));

/// The closure must not call back into this module.
fn with_open<R>(f: impl FnOnce(&mut Open) -> R) -> Option<R> {
    MENU.0.borrow_mut().as_mut().map(f)
}

/// `HUD_Init`: `cl_plugins_menu` (port addition). It only opens the panel.
pub fn init() {
    engine().add_command(c"cl_plugins_menu", cmd_menu);
}

extern "C" fn cmd_menu() {
    open();
}

/// Whether the panel is open.
pub fn is_open() -> bool {
    MENU.0.try_borrow().is_ok_and(|m| m.is_some())
}

/// The rows now, and the note when nothing can be changed; `None` when client plugins aren't set
/// up in this client.
fn current() -> Option<(Vec<Row>, Option<String>)> {
    let loaded = crate::ext::loaded();
    let suspended = crate::ext::suspended();
    super::with(|s| {
        let rows = model::rows(&s.items, &s.store, s.opts.platform, &|it| super::status_text(it, &loaded, &suspended), &|it| {
            loaded.contains(&it.name)
        });
        let note = (!s.store_writable).then(|| {
            format!("{} couldn't be read at start, so nothing can be changed here (see approved.toml.bad).", s.dirs.store().display())
        });
        (rows, note)
    })
}

/// Opens the panel on the list (or, with the e2e hook, answers it).
pub fn open() {
    let Some((rows, note)) = current() else {
        super::say("client plugins aren't set up in this client");
        return;
    };
    #[cfg(feature = "e2e")]
    if let Some(answers) = e2e::answers(&rows) {
        for (id, op) in answers {
            let row = current().and_then(|(rows, _)| rows.into_iter().find(|r| r.id == id && op.offered(r)));
            let result = match (&note, row) {
                (Some(n), _) => n.clone(),
                (None, Some(r)) => change(&r, op).0,
                (None, None) => format!("{id}: {} doesn't apply", op.name()),
            };
            e2e::done(&id, &result);
        }
        return;
    }
    let mut panel = Panel::new(rows);
    panel.note = note;
    *MENU.0.borrow_mut() = Some(Open { panel, arming: Arming::now(), clicks: Clicks::default() });
    super::say("plugins panel open (Escape closes it)");
}

fn close() {
    *MENU.0.borrow_mut() = None;
}

/// Does `op` to the plugin `row` shows, as it is now (the panel's copy may be stale: a server's
/// offer may have installed or loaded something since). The result line, and whether it failed.
fn change(row: &Row, op: Op) -> (String, bool) {
    let same = |r: &Row| r.id == row.id && r.dir == row.dir && r.version == row.version;
    let Some(row) = current().and_then(|(rows, _)| rows.into_iter().find(same)) else {
        return (format!("{} changed while the panel was open; nothing was done", row.id), true);
    };
    if !op.offered(&row) {
        return (format!("{} ({}) can't be changed that way now", row.title(), row.id), true);
    }
    let loaded = crate::ext::loaded();
    let r = super::with(|s| {
        let applied = model::apply(op, &row, &mut s.store, &s.dirs.cl_plugins)?;
        let mut message = applied.message;
        if applied.store_changed
            && let Err(e) = super::save_store(s)
        {
            message += &format!(" (for this game only: {e})");
        }
        after(s, &row, op, &loaded);
        Ok::<String, String>(message)
    });
    let (message, failed) = match r {
        None => ("client plugins aren't set up in this client".into(), true),
        Some(Ok(m)) => (m, false),
        Some(Err(e)) => (e, true),
    };
    super::say(&message);
    (message, failed)
}

/// What start-up decided, brought up to date after a change: a revoked plugin that isn't
/// loaded won't load (a server-scoped one waiting for its server, or one offered again), a
/// deleted version is gone.
fn after(s: &mut super::State, row: &Row, op: Op, loaded: &[String]) {
    match op {
        Op::Revoke => {
            for it in s.items.iter_mut().filter(|it| it.candidate.id() == row.id && !loaded.contains(&it.name)) {
                if matches!(it.decision, Decision::Allowed { .. }) {
                    it.decision = Decision::Skip("approval revoked; asks again at the next start".into());
                }
            }
        }
        Op::Unblock => {
            for it in s.items.iter_mut().filter(|it| it.candidate.id() == row.id) {
                if matches!(it.decision, Decision::Skip(_)) {
                    it.decision = Decision::Skip("unblocked; asks at the next start".into());
                }
            }
        }
        Op::Delete => {
            s.items.retain(|it| !matches!(&it.candidate, plugin_pkg::Candidate::Package { dir } if Some(dir) == row.dir.as_ref()));
            s.by_library = super::startup::by_library(&s.items);
        }
    }
}

// ---- drawing and input --------------------------------------------------------------------------

fn screen_with<R>(f: impl FnOnce(&Screen) -> R) -> R {
    let s = crate::hud::screen();
    let measure = super::offers::measure;
    f(&Screen { width: s.width, height: s.height, line_h: super::offers::line_height(), measure: &measure })
}

/// `HUD_Redraw`, before an open offer card: the panel, and the cursor unless a card is over it.
pub fn redraw(cursor: bool) {
    let Some((built, armed, remaining)) = screen_with(|s| with_open(|m| (m.panel.build(s), m.arming.armed(), m.arming.remaining_secs())))
    else {
        return;
    };
    super::offers::draw_card(&built.layout, &built.buttons, armed, remaining, false);
    if cursor {
        super::offers::draw_cursor();
    }
}

/// Acts on a button (or a paging key); a new view starts the arming delay again (§7.3).
fn act(m: &mut Open, a: Action, next: Option<usize>) -> Effect {
    let before = m.panel.view.clone();
    let e = m.panel.act(a, next);
    if m.panel.view != before {
        m.arming.reset();
        m.clicks.reset();
    }
    e
}

/// Mouse buttons while the panel has the cursor (`IN_MouseEvent`, through `super::mouse`).
pub fn mouse(state: i32, old: i32, x: i32, y: i32) {
    let (pressed, released) = (state & !old, old & !state);
    let effect = screen_with(|s| {
        with_open(|m| {
            let b = m.panel.build(s);
            let at = card::button_at(&b.layout, x, y);
            if pressed & 1 != 0 {
                m.clicks.press(at);
            }
            if released & 1 != 0 {
                let armed = m.arming.armed();
                let a = m.clicks.release_in(&b.buttons, at, armed, false)?;
                return Some((act(m, a, b.next), m.panel.rows.clone()));
            }
            None
        })
        .flatten()
    });
    match effect {
        Some((Effect::Close, _)) => close(),
        Some((Effect::Apply { row, op }, rows)) => {
            let (message, failed) = rows.get(row).map_or_else(|| ("nothing to change".to_string(), true), |r| change(r, op));
            let fresh = current().map(|(rows, _)| rows).unwrap_or_default();
            with_open(|m| {
                m.panel.done(fresh, message, failed);
                m.arming.reset();
                m.clicks.reset();
            });
        }
        _ => {}
    }
}

/// GoldSrc `keydefs.h`.
const K_ESCAPE: i32 = 27;
const K_PGDN: i32 = 149;
const K_PGUP: i32 = 150;
const K_MWHEELDOWN: i32 = 239;
const K_MWHEELUP: i32 = 240;

/// `HUD_Key_Event`, after an offer card's: Escape closes the panel; Page Down / Page Up and the
/// mouse wheel turn its pages. No key changes anything (§7.3).
pub fn key_event(down: bool, key: i32) -> bool {
    if !down || !is_open() {
        return false;
    }
    let a = match key {
        K_ESCAPE => {
            close();
            return true;
        }
        K_PGDN | K_MWHEELDOWN => Action::Next,
        K_PGUP | K_MWHEELUP => Action::Previous,
        _ => return false,
    };
    screen_with(|s| {
        with_open(|m| {
            let next = m.panel.build(s).next;
            act(m, a, next);
        })
    });
    true
}
