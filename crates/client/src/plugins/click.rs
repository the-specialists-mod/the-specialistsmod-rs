//! The click-only safeguards every approval shares (client-plugins.md §7.3): the start-up box
//! ([`super::prompt`]), the in-game offer and update cards ([`super::offers`]), the plugins
//! panel's confirmations ([`super::menu`]) and restart and reconnect's cards
//! (`crate::relaunch::card`, the same delay).
//!
//! * An approving button counts only once its dialog has been up for [`ARM_DELAY`], and the
//!   delay starts again whenever the dialog changes, so a click aimed at something else can't
//!   land on it.
//! * The default (Enter, Escape, closing) is never an approval.
//! * Nothing approves from the console: no command, alias, bind or `stufftext` reaches these.

use std::time::{Duration, Instant};

/// How long a dialog must be up before an approving button counts.
pub const ARM_DELAY: Duration = Duration::from_secs(2);

/// Whether an approval given `shown` after the dialog appeared (or last changed) counts.
pub fn armed(shown: Duration) -> bool {
    shown >= ARM_DELAY
}

/// When a dialog was shown or last changed.
#[derive(Clone, Copy, Debug)]
pub struct Arming {
    since: Instant,
}

impl Arming {
    pub fn now() -> Arming {
        Arming { since: Instant::now() }
    }
    /// The dialog changed: the delay starts again.
    pub fn reset(&mut self) {
        self.since = Instant::now();
    }
    pub fn armed(&self) -> bool {
        armed(self.since.elapsed())
    }
    /// Whole seconds until it's armed (0 once armed), for the disabled buttons' countdown.
    pub fn remaining_secs(&self) -> u64 {
        ARM_DELAY.saturating_sub(self.since.elapsed()).as_secs_f32().ceil() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_delay() {
        assert!(!armed(Duration::from_millis(1999)) && armed(ARM_DELAY));
        let mut a = Arming::now();
        assert!(!a.armed());
        assert_eq!(a.remaining_secs(), 2);
        a.since -= Duration::from_secs(3);
        assert!(a.armed() && a.remaining_secs() == 0);
        a.reset();
        assert!(!a.armed(), "a change starts the delay again");
    }
}
