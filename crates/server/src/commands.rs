//! Client command dispatch (`pfnClientCommand`). Each subsystem gets a chance to handle the
//! command for the player who sent it; the first that returns true wins.

use crate::player::Player;
use crate::world::{self, Cx};
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::ffi::{CStr, CString};

/// A client command's arguments (`argv(0)` is the command name): the engine's current command,
/// or a command a plugin runs for a player (`plugin_api::bot_command`).
pub struct Args {
    src: Source,
}

enum Source {
    /// The engine's `Cmd_Argv`, without `skip` leading arguments: `ts_as <player#> <command> ...`
    /// runs `<command>` with `skip` 2.
    Engine { skip: i32 },
    /// Arguments given as strings; `rest` is everything after the command name.
    Owned { argv: Vec<CString>, rest: CString },
}

impl Args {
    /// The engine's current command.
    pub fn engine() -> Args {
        Args { src: Source::Engine { skip: 0 } }
    }

    /// `line` split into arguments as the engine's `Cmd_TokenizeString` does for a client command:
    /// on white space, with double quotes grouping (and dropped). `None` for an empty line.
    pub fn parse(line: &str) -> Option<Args> {
        let argv = tokenize(line);
        if argv.is_empty() {
            return None;
        }
        let rest = line.trim_start();
        let rest = rest.strip_prefix(argv[0].as_str()).or_else(|| rest.strip_prefix(&format!("\"{}\"", argv[0]))).unwrap_or("");
        let c = |s: &str| CString::new(s.replace('\0', "")).unwrap_or_default();
        Some(Args { src: Source::Owned { argv: argv.iter().map(|a| c(a)).collect(), rest: c(rest.trim_start()) } })
    }

    pub fn argc(&self) -> i32 {
        match &self.src {
            Source::Engine { skip } => (engine().cmd_argc() - skip).max(0),
            Source::Owned { argv, .. } => argv.len() as i32,
        }
    }
    pub fn argv(&self, i: i32) -> &CStr {
        match &self.src {
            Source::Engine { skip } => engine().cmd_argv(i + skip),
            Source::Owned { argv, .. } => usize::try_from(i).ok().and_then(|i| argv.get(i)).map_or(c"", |a| a.as_c_str()),
        }
    }
    pub fn str(&self, i: i32) -> &str {
        self.argv(i).to_str().unwrap_or("")
    }
    /// `strtol(argv(i), NULL, 10)`, as TS reads its numeric client-command arguments (`jointeam`,
    /// `powerup`, `fov`, `VModEnable`): leading white space and digits, so `2abc` is 2.
    pub fn int(&self, i: i32) -> i32 {
        crate::rules::mapcycle::atoi(&self.argv(i).to_string_lossy())
    }
    pub fn float(&self, i: i32) -> f32 {
        self.str(i).trim().parse().unwrap_or(0.0)
    }
    /// Everything after the command name, as the client typed it (for `ts_as`, everything after
    /// `ts_as`).
    pub fn rest(&self) -> &CStr {
        match &self.src {
            Source::Engine { .. } => engine().cmd_args(),
            Source::Owned { rest, .. } => rest,
        }
    }
}

/// `Cmd_TokenizeString`'s splitting (FWGS `COM_ParseFile` without the comment and single-character
/// rules, which client commands don't use): white space separates, a double quote starts an
/// argument that runs to the next double quote.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&c) = chars.peek() else { break };
        let mut arg = String::new();
        if c == '"' {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                arg.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() || c == '"' {
                    break;
                }
                arg.push(c);
                chars.next();
            }
        }
        out.push(arg);
    }
    out
}

/// `ts_as <player#> <command> [args...]` (sv_cheats, port-only dev aid): runs a client command
/// as player `#`, e.g. `ts_as 2 ts_give 2_2` or `ts_as 2 useitems` for a bot. Unknown commands
/// are ignored.
pub extern "C" fn cmd_as() {
    let eng = engine();
    if eng.cvar_float(c"sv_cheats") == 0.0 || eng.cmd_argc() < 3 {
        eng.print("usage (sv_cheats 1): ts_as <player#> <command> [args...]\n");
        return;
    }
    let e = eng.client(eng.cmd_argv(1).to_string_lossy().trim().parse().unwrap_or(0));
    if e.is_null() || !e.has_private_data() {
        return;
    }
    let args = Args { src: Source::Engine { skip: 2 } };
    let cmd = args.str(0);
    world::dispatch(e, |ent, cx| {
        let Some(p) = (ent as &mut dyn std::any::Any).downcast_mut::<Player>() else { return false };
        dispatch_player(p, cx, cmd, &args)
    });
}

/// `ClientCommand` (0xad838). Command names are matched as typed: TS compares them with `strcmp`
/// and `strstr(..) == argv(0)` prefixes, so `Say` or `WEAPON_13` are unknown commands. Only the
/// voice manager's `vban`/`VModEnable` use `strcasecmp`. Without private data (no `Player`) TS
/// returns at once.
pub fn client_command(e: *mut edict_t) {
    run(e, &Args::engine());
}

/// [`client_command`] with given arguments: the engine's, or a plugin's for a player
/// (`plugin_api::bot_command`). Plugins see the command first (port addition: a plugin's
/// `client_command` callback that returns non-zero swallows it).
pub fn run(e: *mut edict_t, args: &Args) {
    if crate::plugins::client_command(e, args) {
        return;
    }
    // Port addition: the client's `tspkg` answers to plugin offers (issue #298).
    if crate::plugin_offers::client_command(e, args) {
        return;
    }
    let cmd = args.str(0);
    let handled = world::dispatch(e, |ent, cx| {
        let Some(p) = (ent as &mut dyn std::any::Any).downcast_mut::<Player>() else { return false };
        dispatch_player(p, cx, cmd, args)
    });
    if handled == Some(false) {
        // ClientCommand's tail (0xae5de): `argv(0)` as typed, cut to 127 bytes, through
        // `ClientPrint(HUD_PRINTCONSOLE)` (a `TextMsg`). The TS client sends `_special` on every
        // +attack2, and TS answers it with this too.
        let typed = args.argv(0).to_bytes();
        let typed = &typed[..typed.len().min(127)];
        crate::rules::msg::text_one(e, shared::protocol::rules::hud_dest::CONSOLE, &shared::bformat!("Unknown command: {}\n", typed));
    }
}

fn dispatch_player(p: &mut Player, cx: &mut Cx, cmd: &str, args: &Args) -> bool {
    // Port addition: a gameplay ruleset decides its own commands first.
    if let Some(handled) = crate::gameplay::active().server.client_command(p, cx, cmd, args) {
        return handled;
    }
    crate::weapons::client_command(p, cx, cmd, args)
        || crate::stunt::client_command(p, cx, cmd, args)
        || crate::rules::client_command(p, cx, cmd, args)
        || crate::bot_backend::client_command(p, cx, cmd, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_like_the_engine() {
        assert_eq!(tokenize("  say  hello world "), ["say", "hello", "world"]);
        assert_eq!(tokenize("say \"hello world\" x"), ["say", "hello world", "x"]);
        assert_eq!(tokenize("jointeam 1"), ["jointeam", "1"]);
        assert_eq!(tokenize("a\"b c\""), ["a", "b c"]);
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn owned_args() {
        let a = Args::parse("say \"hi there\" 2").unwrap();
        assert_eq!(a.argc(), 3);
        assert_eq!(a.str(0), "say");
        assert_eq!(a.str(1), "hi there");
        assert_eq!(a.int(2), 2);
        assert_eq!(a.str(3), "");
        assert_eq!(a.rest().to_str().unwrap(), "\"hi there\" 2");
        assert!(Args::parse(" ").is_none());
    }
}
