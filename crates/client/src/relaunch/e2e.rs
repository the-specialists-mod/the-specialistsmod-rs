//! The relaunch e2e test's hooks (`cargo xtask relaunch`, docs/validation.md), compiled only into
//! `--features e2e` builds and driven only by the environment, never by a console command
//! (docs/implementation/client-plugins.md §11):
//!
//! * `TSRS_E2E_CARDS=<file>`: cards aren't drawn; each is answered from `<file>`, whose lines are
//!   `<kind>=ok|cancel` (`inform`, `reconnect`, `password`) and `typed=<text>` (what the password
//!   field gets), and recorded in `<file>.shown` as `kind=<kind> field=<0|1> <title>: <text>`.
//!   A kind with no line is cancelled.
//! * `TSRS_E2E_SELF_RELAUNCH=1` enables the self-relaunch whatever
//!   [`super::plan::SELF_RELAUNCH_VERIFIED`] says, so the test can verify a platform before it
//!   goes in that list; `0` disables it.

use super::card::{Answer, Card};
use std::io::Write;

pub fn answer(card: &Card) -> Option<Answer> {
    let path = std::env::var_os("TSRS_E2E_CARDS")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut shown = path.clone();
    shown.push(".shown");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&shown) {
        let _ = writeln!(f, "kind={} field={} {}", card.kind.name(), u8::from(card.password), card.summary());
    }
    let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::to_owned);
    Some(match get(card.kind.name()).as_deref() {
        Some("ok") => Answer::Ok(card.password.then(|| get("typed").unwrap_or_default())),
        _ => Answer::Cancel,
    })
}

/// `TSRS_E2E_SELF_RELAUNCH`: `1` forces the self-relaunch on, `0` off (the quit-and-inform
/// scenario, on a platform where it is verified); unset leaves the verified list in charge.
pub fn self_relaunch() -> Option<bool> {
    match std::env::var("TSRS_E2E_SELF_RELAUNCH").ok()?.as_str() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}
