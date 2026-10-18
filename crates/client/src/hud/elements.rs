//! The HUD elements client extensions can hide and locate (port addition, issue #314;
//! docs/implementation/client-ext-api.md §12). Each element's draw code runs inside [`draw`]:
//! while the element is hidden it still runs, as a dry run (`goldsrc::ext::hud_record`), so its
//! state moves on exactly as when it's shown and its rectangle stays current. TS has no such
//! thing; with no extension loaded and nothing hidden, [`draw`] is a plain call.
//!
//! Which code is which element is in `ts_client_ext_api::hud`'s docs.

use goldsrc::Global;
use goldsrc::ext::hud_record;
use std::cell::RefCell;
use ts_client_ext_api::{HudElement, hud, hud_flag};

const COUNT: usize = hud::COUNT as usize;

/// The elements' state: what is hidden, and where each last drew.
#[derive(Clone, Debug, Default)]
pub struct Elements {
    /// The union of the extensions' masks (only extensions that aren't suspended).
    pub hidden: u64,
    /// An extension is loaded: rectangles are worth working out.
    pub tracking: bool,
    /// HUD redraws so far.
    frame: u64,
    /// Where each element last drew, and at which redraw.
    last: [Option<([i32; 4], u64)>; COUNT],
}

impl Elements {
    fn is_hidden(&self, id: u32) -> bool {
        self.hidden & hud::bit(id) != 0
    }

    /// Element `id` drew `bounds` (x, y, w, h; `None` for nothing) at this redraw. An element
    /// drawn from two places in one redraw (the health box around the armour) gets both.
    pub fn drew(&mut self, id: u32, bounds: Option<[i32; 4]>) {
        let frame = self.frame;
        let (Some(b), Some(slot)) = (bounds, self.last.get_mut(id as usize)) else { return };
        let b = match *slot {
            Some((a, f)) if f == frame => {
                let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
                let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
                [x0, y0, x1 - x0, y1 - y0]
            }
            _ => b,
        };
        *slot = Some((b, frame));
    }

    /// A new HUD redraw.
    pub fn next_frame(&mut self) {
        self.frame += 1;
    }

    /// Every element as the API gives it.
    pub fn list(&self) -> Vec<HudElement> {
        (0..hud::COUNT)
            .map(|id| {
                let mut e = HudElement { id, ..Default::default() };
                if self.is_hidden(id) {
                    e.flags |= hud_flag::HIDDEN;
                }
                if let Some((rect, frame)) = self.last[id as usize] {
                    e.flags |= hud_flag::RECT;
                    e.rect = rect;
                    if frame == self.frame {
                        e.flags |= hud_flag::DRAWN;
                    }
                }
                crate::ext::events::fill(&mut e.name, hud::NAMES[id as usize].to_bytes());
                e
            })
            .collect()
    }
}

static STATE: Global<RefCell<Elements>> = Global(RefCell::new(Elements { hidden: 0, tracking: false, frame: 0, last: [None; COUNT] }));

/// The hidden elements' mask (`crate::ext` works it out from the extensions').
pub fn set_hidden(mask: u64) {
    STATE.0.borrow_mut().hidden = mask & hud::ALL;
}

/// Whether rectangles are worth recording (an extension is loaded).
pub fn set_tracking(on: bool) {
    STATE.0.borrow_mut().tracking = on;
}

/// Element `id` is hidden.
pub fn hidden(id: u32) -> bool {
    STATE.0.borrow().is_hidden(id)
}

/// `HUD_Redraw` starts.
pub fn next_frame() {
    STATE.0.borrow_mut().next_frame();
}

/// `ClientExtApi::hud_elements`.
pub fn list() -> Vec<HudElement> {
    STATE.0.borrow().list()
}

/// Runs element `id`'s draw code `f`: drawn, or while hidden a dry run, its bounds kept.
pub fn draw<R>(id: u32, f: impl FnOnce() -> R) -> R {
    let (tracking, hide) = {
        let s = STATE.0.borrow();
        (s.tracking, s.is_hidden(id))
    };
    if !tracking && !hide {
        return f();
    }
    // The state isn't borrowed while `f` runs: element code may draw other elements.
    let (r, bounds) = hud_record::record(hide, f);
    STATE.0.borrow_mut().drew(id, bounds.map(hud_record::Bounds::rect));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_says_hidden_drawn_and_where() {
        let mut e = Elements { hidden: hud::bit(hud::CHAT) | hud::bit(hud::SCOREBOARD), ..Default::default() };
        e.next_frame();
        e.drew(hud::HEALTH, Some([8, 400, 127, 40]));
        e.drew(hud::CHAT, Some([10, 300, 200, 50]));
        e.drew(hud::TIMER, None);
        e.drew(hud::CASH, Some([0, 0, 10, 10]));
        e.drew(hud::CASH, Some([20, 5, 10, 10]));
        let l = e.list();
        assert_eq!(l[hud::CASH as usize].rect, [0, 0, 30, 15], "two draws in one redraw");
        assert_eq!(l.len(), hud::COUNT as usize);
        assert_eq!(l[hud::HEALTH as usize].flags, hud_flag::DRAWN | hud_flag::RECT);
        assert_eq!(l[hud::HEALTH as usize].rect, [8, 400, 127, 40]);
        assert_eq!(l[hud::CHAT as usize].flags, hud_flag::HIDDEN | hud_flag::DRAWN | hud_flag::RECT, "a dry run counts as drawn");
        assert_eq!(l[hud::SCOREBOARD as usize].flags, hud_flag::HIDDEN, "never drawn: no rectangle");
        assert_eq!(l[hud::TIMER as usize].flags, 0);
        let name: Vec<u8> = l[hud::STATUS_ICONS as usize].name.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        assert_eq!(name, b"status_icons");
        // The next redraw: the rectangle stays, "drawn" goes until it draws again.
        e.next_frame();
        let l = e.list();
        assert_eq!(l[hud::HEALTH as usize].flags, hud_flag::RECT);
        assert_eq!(l[hud::HEALTH as usize].rect, [8, 400, 127, 40]);
        e.drew(99, Some([0, 0, 1, 1]));
    }
}
