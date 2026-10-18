//! The offer cards' test hook (`cargo xtask tspkg`, client-plugins.md §11 e2e 2–4), compiled only
//! into `--features e2e` builds and driven only by the environment, never by a console command or
//! anything a server sends (§7.3). It shares `TSRS_E2E_CARDS=<file>` with restart and
//! reconnect's hook (`crate::relaunch`): a card isn't drawn but answered from the file's lines
//! `<kind>:<id>=<answer>` or `<kind>=<answer>` (kinds `offer`, `update`, `port`, `restart`;
//! answers `install`, `server`, `later`, `never`, `never_all`, `update`, `restart`, or `ok` for the
//! card's first button), and recorded in `<file>.shown` as
//! `kind=<kind> id=<id> buttons=<a|b|...> answer=<answer> <text>`. A card with no line is
//! answered "Not now".

use super::card::{Action, Card};
use std::io::Write;

pub fn answer(card: &Card) -> Option<Action> {
    let path = std::env::var_os("TSRS_E2E_CARDS")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let get = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(str::trim).map(str::to_owned);
    let kind = card.kind.name();
    let a = match get(&format!("{kind}:{}", card.id)).or_else(|| get(kind)).as_deref() {
        Some("ok") => card.buttons.first().map_or(Action::Later, |b| b.action),
        Some(other) => Action::parse(other)
            .filter(|a| card.buttons.iter().any(|b| b.action == *a || b.shift.is_some_and(|s| s.1 == *a)))
            .unwrap_or(Action::Later),
        None => Action::Later,
    };
    let mut shown = path;
    shown.push(".shown");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&shown) {
        let _ = writeln!(f, "kind={kind} id={} buttons={} answer={} {}", card.id, card.labels().join("|"), a.name(), card.summary());
    }
    Some(a)
}
