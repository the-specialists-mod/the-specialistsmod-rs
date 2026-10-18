//! Where the HUD draws, and drawing nothing: a port addition for client extensions' HUD control
//! (issue #314). Every 2D HUD primitive ([`ClEngine::spr_draw`](crate::client::ClEngine) and its
//! additive and holes forms, `draw_character`, `draw_console_string`, `draw_console_bytes`,
//! `fill`, `fill_blend`, `center_print_bytes`) tells [`note`] what it covers. Inside
//! [`record`], those rectangles add up to the bounds the caller gets back; with `dry` set they
//! are only counted, not drawn, so a hidden HUD element's code still runs (its state moves on as
//! when it's shown) and its bounds stay current.
//!
//! Outside any [`record`] the primitives draw as always and nothing is kept.

use crate::Global;
use std::cell::Cell;

/// A rectangle's union, in screen pixels: `[x0, y0, x1, y1)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Bounds {
    /// Adds the `w`×`h` rectangle at (`x`, `y`); an empty one adds nothing.
    fn add(acc: Option<Bounds>, x: i32, y: i32, w: i32, h: i32) -> Option<Bounds> {
        if w <= 0 || h <= 0 {
            return acc;
        }
        let r = Bounds { x0: x, y0: y, x1: x.saturating_add(w), y1: y.saturating_add(h) };
        Some(match acc {
            None => r,
            Some(a) => Bounds { x0: a.x0.min(r.x0), y0: a.y0.min(r.y0), x1: a.x1.max(r.x1), y1: a.y1.max(r.y1) },
        })
    }

    /// x, y, width, height.
    pub fn rect(self) -> [i32; 4] {
        [self.x0, self.y0, self.x1 - self.x0, self.y1 - self.y0]
    }
}

#[derive(Clone, Copy, Default)]
struct Rec {
    /// Inside a [`record`].
    on: bool,
    dry: bool,
    bounds: Option<Bounds>,
}

static REC: Global<Cell<Rec>> = Global(Cell::new(Rec { on: false, dry: false, bounds: None }));
/// The sprite the last `SPR_Set` chose, for the size of a draw without a rectangle.
static SPRITE: Global<Cell<crate::sys::HSPRITE>> = Global(Cell::new(0));
/// The console font's height as the last `pfnGetScreenInfo` gave it.
static CHAR_HEIGHT: Global<Cell<i32>> = Global(Cell::new(0));

/// Runs `f`, giving back the bounds of what it drew (`None` for nothing). With `dry`, nothing
/// it draws reaches the screen. A record inside another stands alone: what it draws is its own
/// (not the outer one's), drawn or not by its own `dry`, as one HUD element drawn from inside
/// another's code (the stunt steps inside the crosshair) is still its own element.
pub fn record<R>(dry: bool, f: impl FnOnce() -> R) -> (R, Option<Bounds>) {
    let outer = REC.0.replace(Rec { on: true, dry, bounds: None });
    let r = f();
    let inner = REC.0.replace(outer).bounds;
    (r, inner)
}

/// A primitive covers `w`×`h` at (`x`, `y`): false when it must not draw (a dry run).
pub fn note(x: i32, y: i32, w: i32, h: i32) -> bool {
    let mut r = REC.0.get();
    if !r.on {
        return true;
    }
    r.bounds = Bounds::add(r.bounds, x, y, w, h);
    REC.0.set(r);
    !r.dry
}

/// Inside a [`record`]: the primitives work out their sizes only then.
pub fn recording() -> bool {
    REC.0.get().on
}

/// A primitive that can't say what it covers: false in a dry run.
pub fn may_draw() -> bool {
    !REC.0.get().dry
}

pub(crate) fn set_sprite(h: crate::sys::HSPRITE) {
    SPRITE.0.set(h);
}

pub(crate) fn sprite() -> crate::sys::HSPRITE {
    SPRITE.0.get()
}

pub(crate) fn set_char_height(h: i32) {
    CHAR_HEIGHT.0.set(h);
}

/// The console font's height, or 16 before the first `pfnGetScreenInfo`.
pub(crate) fn char_height() -> i32 {
    match CHAR_HEIGHT.0.get() {
        h if h > 0 => h,
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_dry_runs_and_records_inside_records() {
        assert!(note(0, 0, 5, 5), "outside a record everything draws");
        let ((), b) = record(false, || {
            assert!(note(10, 20, 5, 5));
            assert!(note(30, 2, 10, 1));
            assert!(note(0, 0, 0, 9), "an empty one adds nothing");
        });
        assert_eq!(b.map(Bounds::rect), Some([10, 2, 30, 23]));
        let ((), b) = record(true, || {
            assert!(!note(1, 1, 2, 2), "dry");
            assert!(!may_draw());
        });
        assert_eq!(b.map(Bounds::rect), Some([1, 1, 2, 2]));
        assert!(may_draw() && !recording(), "back to normal");
        assert_eq!(record(false, || ()).1, None, "nothing drawn");

        // A record inside another stands alone. (One test: the recorder is one process-wide
        // state, and tests run on several threads.)
        let (inner, outer) = record(false, || {
            note(0, 0, 1, 1);
            let ((), inner) = record(true, || {
                assert!(!note(50, 50, 10, 10));
            });
            assert!(note(2, 2, 1, 1), "the outer one isn't dry");
            inner
        });
        assert_eq!(inner.map(Bounds::rect), Some([50, 50, 10, 10]));
        assert_eq!(outer.map(Bounds::rect), Some([0, 0, 3, 3]), "the inner record's draws are its own");
        let ((), _) = record(true, || {
            let ((), _) = record(false, || assert!(note(0, 0, 1, 1), "drawn by its own flag"));
            assert!(!note(0, 0, 1, 1), "the outer one is dry again");
        });
    }
}
