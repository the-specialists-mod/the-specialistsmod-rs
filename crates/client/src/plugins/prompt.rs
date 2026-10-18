//! The start-up prompt (client-plugins.md §7.1, §7.3): one box per new or changed plugin.
//!
//! * [`SdlPrompter`]: `SDL_ShowMessageBox` in the SDL2 the engine loaded. Enter and Escape are
//!   "Not now". "Allow" counts only once the box has been up for [`ARM_DELAY`]: an earlier Allow
//!   shows the box again, so a click or key aimed at something else can't approve.
//! * [`Hook`]: the test hook. In a **debug build only**, [`HOOK_ENV`] answers the boxes without
//!   showing them, and [`HOOK_LOG_ENV`] names a file (relative to the game directory) that
//!   records what each box said. A release build ignores both and says so. It's an environment
//!   variable, never a console command, so a server can't set it (§7.3).
//! * With neither (no SDL2, or the box fails), [`Fallback`] answers "Not now" and says how to
//!   approve instead.

pub use super::click::{ARM_DELAY, armed};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

/// The test hook's answers (debug builds only): `id=answer,...`, with `*` for the rest;
/// answers `allow`, `later`, `later_all`, `never`.
pub const HOOK_ENV: &str = "TSRS_DEV_PLUGIN_PROMPT";
/// The test hook's record of each box, relative to the game directory (debug builds only).
pub const HOOK_LOG_ENV: &str = "TSRS_DEV_PLUGIN_PROMPT_LOG";

/// One box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// The plugin id (`manual:<name>` for a hand-installed library).
    pub id: String,
    pub title: String,
    pub text: String,
    /// False when the plugin can't be approved as installed: no "Allow" (§5.2 step 5).
    pub can_allow: bool,
    /// "Not now for all": more than three plugins to ask about.
    pub show_later_all: bool,
}

/// The player's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Allow,
    /// "Not now": doesn't load, asks again at the next start. The default.
    Later,
    /// "Not now for all": this one and every remaining box.
    LaterAll,
    /// "Never": blocked.
    Never,
}

impl Answer {
    pub fn parse(s: &str) -> Option<Answer> {
        Some(match s.trim() {
            "allow" => Answer::Allow,
            "later" | "notnow" => Answer::Later,
            "later_all" => Answer::LaterAll,
            "never" => Answer::Never,
            _ => return None,
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Answer::Allow => "allow",
            Answer::Later => "later",
            Answer::LaterAll => "later_all",
            Answer::Never => "never",
        }
    }
}

/// Asks the player about one plugin.
pub trait Prompter {
    fn ask(&mut self, p: &Prompt) -> Answer;
}

/// The buttons a box shows, left to right: (answer, label).
pub fn buttons(p: &Prompt) -> Vec<(Answer, &'static str)> {
    let mut b = Vec::new();
    if p.can_allow {
        b.push((Answer::Allow, "Allow"));
    }
    b.push((Answer::Later, "Not now"));
    if p.show_later_all {
        b.push((Answer::LaterAll, "Not now for all"));
    }
    b.push((Answer::Never, "Never"));
    b
}

/// The SDL message box.
pub struct SdlPrompter;

impl Prompter for SdlPrompter {
    fn ask(&mut self, p: &Prompt) -> Answer {
        use goldsrc::platform::sdl::{BoxButton, BoxKind, message_box};
        let list = buttons(p);
        let mut text = p.text.clone();
        loop {
            let b: Vec<BoxButton> = list
                .iter()
                .enumerate()
                .map(|(i, (a, label))| BoxButton { id: i as i32, text: label, enter: *a == Answer::Later, escape: *a == Answer::Later })
                .collect();
            let kind = if p.can_allow { BoxKind::Warning } else { BoxKind::Error };
            let shown = Instant::now();
            let pressed = message_box(kind, &p.title, &text, &b);
            let answer = pressed.and_then(|i| list.get(usize::try_from(i).ok()?)).map_or(Answer::Later, |(a, _)| *a);
            if answer == Answer::Allow && !armed(shown.elapsed()) {
                text = format!("\"Allow\" counts only once this box has been open for {} seconds.\n\n{}", ARM_DELAY.as_secs(), p.text);
                continue;
            }
            return answer;
        }
    }
}

/// Whether SDL's message box is there to ask with.
pub fn sdl_available() -> bool {
    goldsrc::platform::sdl::has("SDL_ShowMessageBox")
}

/// No way to ask: "Not now", with how to approve instead (once).
pub struct Fallback {
    pub told: bool,
    pub lines: Vec<String>,
}

impl Prompter for Fallback {
    fn ask(&mut self, p: &Prompt) -> Answer {
        if !self.told {
            self.told = true;
            self.lines.push("can't show the approval prompt (no SDL2 message box): approve plugins in tsmod-launcher".into());
        }
        self.lines.push(format!(
            "{} needs approval: {}",
            p.id,
            p.text.lines().filter(|l| l.starts_with("  - ")).collect::<Vec<_>>().join(";")
        ));
        Answer::Later
    }
}

/// The test hook (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    /// `(id, answer)`; `*` matches any id. The first match wins.
    pub answers: Vec<(String, Answer)>,
    /// Where each box is recorded.
    pub log: Option<PathBuf>,
}

/// The hook `env` and `log` (the variables' values) describe, in a build with `debug` assertions
/// (pass `cfg!(debug_assertions)`): `None` in a release build or when `env` is unset, and the
/// note for the console. Unknown answers make the hook answer "Not now" for that id.
pub fn hook(env: Option<&str>, log: Option<&str>, game_dir: &Path, debug: bool) -> (Option<Hook>, Option<String>) {
    let Some(env) = env.filter(|e| !e.trim().is_empty()) else { return (None, None) };
    if !debug {
        return (None, Some(format!("{HOOK_ENV} is ignored in release builds")));
    }
    let answers = env
        .split(',')
        .filter_map(|pair| {
            let (id, a) = pair.split_once('=')?;
            Some((id.trim().to_string(), Answer::parse(a).unwrap_or(Answer::Later)))
        })
        .collect();
    let log = log.filter(|l| !l.is_empty()).map(|l| game_dir.join(l));
    (Some(Hook { answers, log }), Some(format!("{HOOK_ENV}: answering the start-up prompts (debug build only)")))
}

impl Prompter for Hook {
    fn ask(&mut self, p: &Prompt) -> Answer {
        let mut answer = self.answers.iter().find(|(id, _)| id == &p.id || id == "*").map_or(Answer::Later, |(_, a)| *a);
        if answer == Answer::Allow && !p.can_allow {
            answer = Answer::Later;
        }
        if answer == Answer::LaterAll && !p.show_later_all {
            answer = Answer::Later;
        }
        if let Some(log) = &self.log {
            let labels: Vec<&str> = buttons(p).iter().map(|(_, l)| *l).collect();
            let entry =
                format!("--- prompt {}\ntitle: {}\nbuttons: {}\n{}answer: {}\n", p.id, p.title, labels.join(", "), p.text, answer.name());
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                let _ = f.write_all(entry.as_bytes());
            }
        }
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(id: &str, can_allow: bool) -> Prompt {
        Prompt { id: id.into(), title: "t".into(), text: "  - new plugin\n".into(), can_allow, show_later_all: false }
    }

    #[test]
    fn buttons_and_defaults() {
        let labels = |p: &Prompt| buttons(p).iter().map(|(_, l)| *l).collect::<Vec<_>>();
        assert_eq!(labels(&prompt("a", true)), ["Allow", "Not now", "Never"]);
        assert_eq!(labels(&prompt("a", false)), ["Not now", "Never"], "no Allow for what can't be approved");
        let many = Prompt { show_later_all: true, ..prompt("a", true) };
        assert_eq!(labels(&many), ["Allow", "Not now", "Not now for all", "Never"]);
        assert!(!armed(Duration::from_millis(1999)) && armed(ARM_DELAY));
    }

    #[test]
    fn the_hook_is_debug_only() {
        let dir = Path::new("/g/ts");
        let (h, note) = hook(Some("*=allow"), Some("x.log"), dir, false);
        assert!(h.is_none(), "release builds ignore the hook");
        assert!(note.unwrap().contains("ignored in release builds"));
        assert_eq!(hook(None, Some("x.log"), dir, true), (None, None));
        let (h, note) = hook(Some("manual:a=never, b=allow,*=later"), Some("x.log"), dir, true);
        assert!(note.unwrap().contains("debug build only"));
        let h = h.unwrap();
        assert_eq!(h.log.as_deref(), Some(Path::new("/g/ts/x.log")));
        assert_eq!(h.answers[0], ("manual:a".into(), Answer::Never));
        assert_eq!(h.answers[2], ("*".into(), Answer::Later));
    }

    #[test]
    fn the_hook_answers_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let (h, _) = hook(Some("a=allow,b=allow,*=never"), Some("prompts.log"), dir.path(), true);
        let mut h = h.unwrap();
        assert_eq!(h.ask(&prompt("a", true)), Answer::Allow);
        assert_eq!(h.ask(&prompt("b", false)), Answer::Later, "no Allow to press");
        assert_eq!(h.ask(&prompt("c", true)), Answer::Never);
        let log = std::fs::read_to_string(dir.path().join("prompts.log")).unwrap();
        assert!(log.contains("--- prompt a\ntitle: t\nbuttons: Allow, Not now, Never\n  - new plugin\nanswer: allow\n"), "{log}");
        assert!(log.contains("--- prompt b\ntitle: t\nbuttons: Not now, Never\n"));
    }

    #[test]
    fn the_fallback_never_approves() {
        let mut f = Fallback { told: false, lines: Vec::new() };
        assert_eq!(f.ask(&prompt("a", true)), Answer::Later);
        assert_eq!(f.ask(&prompt("b", true)), Answer::Later);
        assert_eq!(f.lines.len(), 3);
    }
}
