//! The plugins panel's test hook (`cargo xtask tspkg`, client-plugins.md §11), compiled only into
//! `--features e2e` builds and driven only by the environment, like the offer cards' hook
//! (`super::super::offers::e2e`), never by a console argument or anything a server sends. With
//! `TSRS_E2E_CARDS=<file>`, opening the panel doesn't draw it: each row is recorded in
//! `<file>.shown` as `kind=menu id=<id> buttons=<a|b|...> answer=<op|none> files=<n> <summary>`,
//! the file's lines `menu:<id>=<revoke|unblock|delete>` are done as the confirmation's click does
//! them (recorded as `kind=menu-done id=<id> <result>`), and the panel closes.

use super::model::{Op, Row};
use std::io::Write;

fn shown_line(text: &str) {
    let Some(mut path) = std::env::var_os("TSRS_E2E_CARDS") else { return };
    path.push(".shown");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{text}");
    }
}

/// The changes the file asks for, in row order; `None` without the hook.
pub fn answers(rows: &[Row]) -> Option<Vec<(String, Op)>> {
    let path = std::env::var_os("TSRS_E2E_CARDS")?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = Vec::new();
    for r in rows {
        let key = format!("menu:{}=", r.id);
        let op = text.lines().find_map(|l| l.strip_prefix(&key)).and_then(Op::parse).filter(|o| o.offered(r));
        let buttons: Vec<&str> = [Op::Revoke, Op::Unblock, Op::Delete].into_iter().filter(|o| o.offered(r)).map(Op::name).collect();
        shown_line(&format!(
            "kind=menu id={} buttons={} answer={} files={} {}",
            r.id,
            buttons.join("|"),
            op.map_or("none", Op::name),
            r.files.len(),
            r.summary()
        ));
        if let Some(op) = op {
            out.push((r.id.clone(), op));
        }
    }
    Some(out)
}

pub fn done(id: &str, result: &str) {
    shown_line(&format!("kind=menu-done id={id} {result}"));
}
