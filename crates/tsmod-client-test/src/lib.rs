//! `tsmod_test` (client), the stub client extension: a test fixture for `cargo xtask
//! client-plugins` (issue #293, docs/implementation/client-ext-api.md §9). xtask installs it as
//! `cl_dlls/tsmod_test<suffix>` and, under other names, as fixtures the loader must skip:
//! `tsmod_test_badver` (another API version), `tsmod_test_badsize` (a wrong descriptor size),
//! `tsmod_test_refuse` (its load function refuses) and `tsmod_unapproved` (the approval gate must
//! never let it load; it says so loudly if it does).
//!
//! As `test` it fills every callback, registers `tsext_test_cvar`, `tsext_test_spawn` and
//! `tsext_test_report`, and records each check of [`CHECKS`] the first time it holds. The
//! report command prints `[tsmod_test] check <what> ok|FAIL ...` lines and writes them to
//! `report.txt` in its data directory, which xtask reads (the engine log can sit in a buffer
//! under Wine). `tsext_test_spawn` starts the helper (`tsmod_test_helper`, listed in
//! `cl_dlls/tsmod_test.files`) through `verified_spawn`, which writes `spawned.txt` there, and
//! checks that a file outside the approval is refused.
//!
//! Minor 1 (#294, #295): it claims the `TSExt` channel `test`; `tsext_test_tsext` sends the server
//! `tsext_echo hello`, which the server stub plugin (`crates/tsmod-test`, installed in `dlls/` by
//! xtask) answers on that channel. Its `message_pre` blocks the `SayText` with `tsext_block_me`,
//! holds the one with `tsext_hold_me` for 20 frames, and rewrites each `DeathMsg`'s weapon;
//! `tsext_test_arm` makes `command_pre` block the port's `special` and `server_cmd_pre` watch or
//! rewrite the `_special` it sends.
//!
//! Minor 2 (#306, #314): `tsext_test_hud hide|show` hides the health box and chat
//! (`hud_set_hidden`) and checks at the next redraws that `hud_elements` still places the hidden
//! health box; `tsext_test_menu take|bad|close|giveback` takes over the next `MOTD` (the server's
//! answer to `motd`), checks `menu_reply`'s refusals and its answer, and gives a takeover back
//! (`menu_shown` says 0) for the port to show; `+duck` must set `local::DUCKING`, and a killed
//! player in view must leave `visible_players`.
//!
//! Under any name starting with `quiet` it is a minimal extension for the approval tests (issue
//! #297): it loads, prints `quiet stub init` from `init`, and, installed as a package with a
//! helper, starts that helper through `verified_spawn`. For a server's block (#302) it prints
//! `quiet stub suspended: <reason>` and `quiet stub resumed after <n> callbacks while suspended`
//! (its `frame` and `redraw` count any that come; none may).

#![warn(clippy::undocumented_unsafe_blocks)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
use ts_client_ext_api::{
    AimInfo, ClientExt, ClientExtApi, CommandPre, EXISTS, GameEvent, HudElement, INVALID, LoadInfo, LocalState, MINOR, MessagePre,
    NAME_LEN, NOT_FOUND, OK, PlayerScore, REFUSED, TraceInfo, UserCmd, VERSION, ViewInfo, VisiblePlayer, allow, button, event, hit, hud,
    hud_flag, local, menu, menu_closed, player, renderer, verdict,
};

/// The key `cargo xtask client-plugins` presses with `ts_dev_key` (`K_AUX32`, unbound): the stub
/// swallows it.
pub const TEST_KEY: c_int = 238;

/// The checks the stub reports, in order. xtask requires each to be `ok`.
pub const CHECKS: [&str; 65] = [
    "load",
    "init",
    "add_command",
    "add_command_exists",
    "register_cvar",
    "register_cvar_exists",
    "vid_init",
    "frame",
    "redraw",
    "wants_cursor",
    "key_event",
    "usercmd",
    "usercmd_attack",
    "usercmd_use",
    "usercmd_released",
    "server_changed",
    "event_map",
    "event_spawn",
    "event_health",
    "event_weapon",
    "event_death",
    "suspend",
    "resume",
    "suspended_quiet",
    "local_state",
    "player",
    "scores",
    "aim",
    "trace_line",
    "visible_players",
    "view",
    "world_to_screen",
    "weapon_name",
    "cvar",
    "host_strings",
    "renderer",
    "client_cmd",
    "data_dir",
    "spawn_refused",
    "spawn_started",
    "mouse",
    // Minor 1: `TSExt` (#294), interception (#295).
    "claim_channel",
    "tsext_roundtrip",
    "tsext_unsupported_bot",
    "tsext_broadcast",
    "tsext_too_long",
    "tsext_bad_channel",
    "client_port_version",
    "message_observe",
    "message_unreachable",
    "message_block",
    "message_rewrite",
    "message_hold",
    "release_again",
    "command_protected",
    "command_block",
    "server_cmd_pre",
    // Minor 2: dead players and ducking (#306), HUD control and menu takeover (#314).
    "local_ducking",
    "visible_players_dead",
    "hud_set_hidden",
    "hud_hidden_kept",
    "hud_elements",
    "menu_reply_refused",
    "menu_reply",
    "menu_given_back",
];

/// What `tsext_test_arm <mode>` sets up for the port's `special` command and the `_special` it
/// sends the server.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Arm {
    #[default]
    None,
    /// `command_pre` blocks `special`: no `_special` may follow.
    Block,
    /// Both pass: `server_cmd_pre` sees `_special`.
    Pass,
    /// `server_cmd_pre` rewrites `_special` as `kill`, which the port refuses (a new name).
    Rename,
}

static API: AtomicPtr<ClientExtApi> = AtomicPtr::new(std::ptr::null_mut());

#[derive(Default)]
struct State {
    id: c_int,
    checks: BTreeMap<&'static str, (bool, String)>,
    frames: u32,
    redraws: u32,
    cursor_polls: u32,
    cmds: u32,
    suspended: bool,
    /// Callbacks that arrived while suspended (none may).
    while_suspended: u32,
    key_down: bool,
    key_up: bool,
    /// The library's path (quiet stubs).
    path: String,
    arm: Arm,
    special_blocked: bool,
    special_sent_while_blocked: u32,
    /// The held `SayText`'s token and the frames since.
    held: Option<(u32, u32)>,
    /// Messages `message_pre` saw, and how many of them it must never see (`TSExt`, `TSPkg`,
    /// `SrvSett`).
    messages_seen: u32,
    unreachable_seen: u32,
    broadcast_seen: bool,
    /// Minor 2: commands with `+duck` held; a killed player to look for and the frames since;
    /// redraws since the HUD was hidden; take over the next `MOTD`; give a takeover back.
    duck_cmds: u32,
    death_watch: Option<(c_int, u32)>,
    hud_watch: Option<u32>,
    take_motd: bool,
    give_back: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut s = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(s.get_or_insert_with(State::default))
}

fn api() -> &'static ClientExtApi {
    // SAFETY: set once in the load function to the client's static table, which lives for the
    // process; only read after that (every callback comes after the load).
    unsafe { &*API.load(Ordering::Relaxed) }
}

fn id() -> c_int {
    with(|s| s.id)
}

fn print(msg: &str) {
    let c = CString::new(msg).unwrap_or_default();
    // SAFETY: a table function, called with this extension's id and a NUL-terminated string.
    unsafe { (api().print)(id(), c.as_ptr()) };
}

/// Records a check: the first time it holds it is `ok` for good; until then the latest failure
/// is kept for the report.
fn check(what: &'static str, ok: bool, detail: impl FnOnce() -> String) {
    with(|s| {
        let e = s.checks.entry(what).or_insert((false, "not reached".into()));
        if ok {
            *e = (true, String::new());
        } else if !e.0 {
            e.1 = detail();
        }
    });
}

/// A table function's string result.
fn get_string(f: unsafe extern "C" fn(*mut c_char, c_int) -> c_int) -> String {
    let mut buf = [0 as c_char; 512];
    // SAFETY: a table function, called with a writable buffer of the length given.
    unsafe { f(buf.as_mut_ptr(), buf.len() as c_int) };
    // SAFETY: the table always NUL-terminates what it writes into a non-empty buffer.
    unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned()
}

fn cstr_field<const N: usize>(f: &[c_char; N]) -> String {
    f.iter().take_while(|&&c| c != 0).map(|&c| c as u8 as char).collect()
}

fn helper_name() -> &'static str {
    if cfg!(windows) { "tsmod_test_helper.exe" } else { "tsmod_test_helper" }
}

/// `TSRS_ClientExtLoad`.
///
/// # Safety
/// Called by the client's loader with its table, a `LoadInfo` and a descriptor of `out_size`
/// bytes, all valid for the call (the API's contract).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn TSRS_ClientExtLoad(
    table: *const ClientExtApi,
    api_size: u32,
    info: *const LoadInfo,
    out: *mut ClientExt,
    out_size: u32,
) -> c_int {
    if table.is_null() || info.is_null() || out.is_null() || out_size as usize != size_of::<ClientExt>() {
        return INVALID;
    }
    // SAFETY: the loader's arguments are valid for the call (checked non-NULL above).
    let (t, info) = unsafe { (&*table, &*info) };
    if api_size as usize != size_of::<ClientExtApi>() || t.size != api_size || t.version != VERSION {
        return INVALID;
    }
    API.store(table.cast_mut(), Ordering::Relaxed);
    // SAFETY: `name` is a NUL-terminated string valid for the call.
    let name = unsafe { CStr::from_ptr(info.name) }.to_string_lossy().into_owned();
    // SAFETY: `out` is a descriptor of `out_size` bytes (checked above).
    let out = unsafe { &mut *out };
    match name.as_str() {
        "test_badver" => {
            *out = ClientExt { api_version: VERSION + 1, ..ClientExt::EMPTY };
            return OK;
        }
        "test_badsize" => {
            *out = ClientExt { size: 8, ..ClientExt::EMPTY };
            return OK;
        }
        "test_refuse" => return INVALID,
        "test" => {}
        quiet if quiet.starts_with("quiet") => {
            // SAFETY: `path` is a NUL-terminated string valid for the call.
            let path = unsafe { CStr::from_ptr(info.path) }.to_string_lossy().into_owned();
            with(|s| {
                s.id = info.id;
                s.path = path;
            });
            *out = ClientExt {
                name: c"quiet client stub".as_ptr(),
                version: c"1".as_ptr(),
                init: Some(quiet_init),
                frame: Some(quiet_frame),
                redraw: Some(quiet_redraw),
                suspend: Some(quiet_suspend),
                resume: Some(quiet_resume),
                ..ClientExt::EMPTY
            };
            return OK;
        }
        other => {
            with(|s| s.id = info.id);
            print(&format!("FAIL: loaded as {other}, which the approval gate must never let load"));
            return INVALID;
        }
    }
    with(|s| s.id = info.id);
    let reserved_clear = t.reserved.iter().all(Option::is_none);
    let minor1 = t.claim_channel.is_some() && t.release.is_some();
    let minor2 = t.hud_set_hidden.is_some() && t.hud_elements.is_some() && t.menu_reply.is_some();
    check("load", t.minor == MINOR && reserved_clear && minor1 && minor2, || {
        format!("minor {} reserved clear {reserved_clear} minor 1 {minor1} minor 2 {minor2}", t.minor)
    });
    *out = ClientExt {
        name: c"tsmod_test (client stub)".as_ptr(),
        version: c"1".as_ptr(),
        init: Some(init),
        vid_init: Some(vid_init),
        frame: Some(frame),
        redraw: Some(redraw),
        key_event: Some(key_event),
        mouse_active: Some(mouse_active),
        wants_cursor: Some(wants_cursor),
        usercmd: Some(usercmd),
        game_event: Some(game_event),
        suspend: Some(suspend),
        resume: Some(resume),
        server_changed: Some(server_changed),
        shutdown: Some(shutdown),
        user_message: Some(user_message),
        message_pre: Some(message_pre),
        command_pre: Some(command_pre),
        server_cmd_pre: Some(server_cmd_pre),
        menu_shown: Some(menu_shown),
        menu_closed: Some(menu_closed_cb),
        ..ClientExt::EMPTY
    };
    OK
}

/// A quiet stub's `init`: says it ran (late, for a plugin approved for one server only). A
/// package's copy (`files/lib/<platform>/tsmod_<id>`) also starts `files/bin/<helper>` through
/// `verified_spawn` when the package has one, and tries a file outside its approval.
extern "C" fn quiet_init() {
    print("quiet stub init");
    let path = with(|s| s.path.clone());
    let Some(files) = std::path::Path::new(&path).parent().and_then(|p| p.parent()).and_then(|p| p.parent()) else { return };
    let helper = files.join("bin").join(helper_name());
    if !helper.is_file() {
        return;
    }
    let a = api();
    let helper = CString::new(helper.to_string_lossy().into_owned()).unwrap_or_default();
    let outside = c"liblist.gam";
    let arg = c"from-quiet";
    let argv = [arg.as_ptr()];
    // SAFETY: table functions, called with this extension's id, NUL-terminated paths and an
    // argument array of the length given.
    let (refused, started) = unsafe {
        ((a.verified_spawn)(id(), outside.as_ptr(), 0, std::ptr::null()), (a.verified_spawn)(id(), helper.as_ptr(), 1, argv.as_ptr()))
    };
    print(&format!("quiet spawn refused {} started {}", refused == REFUSED, started > 0));
}

/// A quiet stub's `frame` and `redraw`: only counted, if they come while it's suspended.
extern "C" fn quiet_frame(_time: f64) {
    quiet();
}

extern "C" fn quiet_redraw(_time: f32, _intermission: c_int) {
    quiet();
}

/// A quiet stub's `suspend`: says so with the reason (a server's block, `cargo xtask tspkg`).
unsafe extern "C" fn quiet_suspend(reason: *const c_char) {
    let why = if reason.is_null() {
        String::new()
    } else {
        // SAFETY: a NUL-terminated string valid for the call (checked non-NULL).
        unsafe { CStr::from_ptr(reason) }.to_string_lossy().into_owned()
    };
    with(|s| {
        s.suspended = true;
        s.while_suspended = 0;
    });
    print(&format!("quiet stub suspended: {why}"));
}

extern "C" fn quiet_resume() {
    let n = with(|s| {
        s.suspended = false;
        s.while_suspended
    });
    print(&format!("quiet stub resumed after {n} callbacks while suspended"));
}

/// Counts a callback that came while suspended.
fn quiet() -> bool {
    with(|s| {
        if s.suspended {
            s.while_suspended += 1;
        }
        s.suspended
    })
}

extern "C" fn init() {
    let a = api();
    let me = id();
    // SAFETY: table functions, called with this extension's id, static NUL-terminated names and
    // this crate's command functions.
    let (c1, c2, c3) = unsafe {
        (
            (a.add_command)(me, c"tsext_test_report".as_ptr(), cmd_report),
            (a.add_command)(me, c"tsext_test_spawn".as_ptr(), cmd_spawn),
            (a.add_command)(me, c"tsext_test_report".as_ptr(), cmd_report),
        )
    };
    check("init", true, String::new);
    check("add_command", c1 == OK && c2 == OK, || format!("{c1} {c2}"));
    check("add_command_exists", c3 == EXISTS, || format!("{c3}"));
    // SAFETY: as above.
    let (v1, v2, v3) = unsafe {
        (
            (a.register_cvar)(me, c"tsext_test_cvar".as_ptr(), c"0".as_ptr(), 0),
            (a.register_cvar)(me, c"tsext_test_cvar".as_ptr(), c"1".as_ptr(), 0),
            (a.register_cvar)(me, c"fps_max".as_ptr(), c"1".as_ptr(), 0),
        )
    };
    check("register_cvar", v1 == OK, || format!("{v1}"));
    check("register_cvar_exists", v2 == EXISTS && v3 == EXISTS, || format!("{v2} {v3}"));
    // SAFETY: as above.
    unsafe {
        (a.add_command)(me, c"tsext_test_tsext".as_ptr(), cmd_tsext);
        (a.add_command)(me, c"tsext_test_arm".as_ptr(), cmd_arm);
        (a.add_command)(me, c"tsext_test_hud".as_ptr(), cmd_hud);
        (a.add_command)(me, c"tsext_test_menu".as_ptr(), cmd_menu);
    }
    let Some(claim) = a.claim_channel else { return };
    // SAFETY: a minor-1 table function, called with ids and NUL-terminated names.
    let (k1, k2, k3, k4) = unsafe {
        (claim(me, c"test".as_ptr()), claim(me, c"test".as_ptr()), claim(me, c"bad name".as_ptr()), claim(me + 1000, c"other".as_ptr()))
    };
    check("claim_channel", k1 == OK && k2 == OK && k3 == INVALID && k4 == INVALID, || format!("{k1} {k2} {k3} {k4}"));
}

/// `tsext_test_tsext`: the extension's command to the server (the reverse direction): the server
/// stub plugin answers `tsext_echo` on the `TSExt` channel `test` ([`user_message`]).
extern "C" fn cmd_tsext() {
    // SAFETY: a table function, called with this extension's id and a NUL-terminated line.
    let rc = unsafe { (api().server_cmd)(id(), c"tsext_echo hello".as_ptr()) };
    if rc != OK {
        print(&format!("tsext_echo not sent: {rc}"));
    }
}

/// `tsext_test_arm none|block|pass|rename` ([`Arm`]).
extern "C" fn cmd_arm() {
    let a = api();
    // SAFETY: `cmd_argv` returns a NUL-terminated string valid until the next command.
    let mode = unsafe { CStr::from_ptr((a.cmd_argv)(1)) }.to_string_lossy().into_owned();
    let arm = match mode.as_str() {
        "block" => Arm::Block,
        "pass" => Arm::Pass,
        "rename" => Arm::Rename,
        _ => Arm::None,
    };
    with(|s| s.arm = arm);
}

/// The first argument of the running command.
fn arg1() -> String {
    // SAFETY: `cmd_argv` returns a NUL-terminated string valid until the next command.
    unsafe { CStr::from_ptr((api().cmd_argv)(1)) }.to_string_lossy().into_owned()
}

/// `tsext_test_hud hide|show` (minor 2): hides the health box and chat, or shows everything.
extern "C" fn cmd_hud() {
    let Some(set) = api().hud_set_hidden else { return };
    if arg1() == "hide" {
        // SAFETY: a minor-2 table function, called with this extension's id.
        let (rc, bad) = unsafe { (set(id(), hud::bit(hud::HEALTH) | hud::bit(hud::CHAT)), set(id(), 1 << 40)) };
        check("hud_set_hidden", rc == OK && bad == INVALID, || format!("{rc} {bad}"));
        with(|s| s.hud_watch = Some(0));
    } else {
        // SAFETY: as above.
        unsafe { set(id(), 0) };
    }
}

/// The HUD elements (minor 2).
fn hud_elements() -> Vec<HudElement> {
    let Some(f) = api().hud_elements else { return Vec::new() };
    let mut out = [HudElement::default(); 32];
    // SAFETY: a minor-2 table function, called with room for `out.len()` elements.
    let n = unsafe { f(out.as_mut_ptr(), out.len() as c_int) };
    out[..(n.max(0) as usize).min(out.len())].to_vec()
}

/// A minor-2 `menu_reply`.
fn reply(kind: c_int, choice: &CStr) -> c_int {
    match api().menu_reply {
        // SAFETY: a table function, called with this extension's id and a NUL-terminated choice.
        Some(f) => unsafe { f(id(), kind, choice.as_ptr()) },
        None => INVALID,
    }
}

/// `tsext_test_menu take|bad|close|giveback` (minor 2): take over the next `MOTD`; the replies the
/// port must refuse; close it; give it back to the port.
extern "C" fn cmd_menu() {
    match arg1().as_str() {
        "take" => with(|s| s.take_motd = true),
        "bad" => {
            let (only_closes, no_text, no_kind) = (reply(menu::MOTD, c"1"), reply(menu::TEXT, c"1"), reply(99, c""));
            check("menu_reply_refused", only_closes == INVALID && no_text == NOT_FOUND && no_kind == INVALID, || {
                format!("{only_closes} {no_text} {no_kind}")
            });
        }
        "close" => {
            let (first, again) = (reply(menu::MOTD, c""), reply(menu::MOTD, c""));
            check("menu_reply", first == OK && again == NOT_FOUND, || format!("{first} {again}"));
        }
        "giveback" => with(|s| s.give_back = true),
        other => print(&format!("tsext_test_menu: {other}?")),
    }
}

/// `menu_shown` (minor 2): it shows what it took over until `tsext_test_menu giveback`.
extern "C" fn menu_shown(_kind: c_int) -> c_int {
    (!with(|s| s.give_back)) as c_int
}

/// `menu_closed` (minor 2).
extern "C" fn menu_closed_cb(kind: c_int, reason: c_int) {
    print(&format!("menu {kind} closed: {reason}"));
    if kind == menu::MOTD && reason == menu_closed::GIVEN_BACK && with(|s| std::mem::take(&mut s.give_back)) {
        check("menu_given_back", true, String::new);
    }
}

/// The minor-1 `release` (the table has it: `load` checks).
fn release(token: u32, verdict: c_int) -> c_int {
    match api().release {
        // SAFETY: a table function, called with this extension's id and no data.
        Some(f) => unsafe { f(id(), token, verdict, std::ptr::null(), 0) },
        None => INVALID,
    }
}

unsafe extern "C" fn user_message(channel: *const c_char, data: *const u8, len: c_int) {
    if quiet() || channel.is_null() || (data.is_null() && len != 0) || len < 0 {
        return;
    }
    // SAFETY: the client passes a NUL-terminated channel and `len` bytes, valid for the call.
    let (channel, data) =
        unsafe { (CStr::from_ptr(channel), if len == 0 { &[][..] } else { std::slice::from_raw_parts(data, len as usize) }) };
    if channel != c"test" {
        print(&format!("FAIL: a message on channel {channel:?}, which it never claimed"));
        return;
    }
    let text = String::from_utf8_lossy(data).into_owned();
    if text == "broadcast" {
        with(|s| s.broadcast_seen = true);
        return;
    }
    let Some(rest) = text.strip_prefix("echo:") else { return };
    let field = |k: &str| rest.split(' ').find_map(|w| w.strip_prefix(&format!("{k}="))).unwrap_or("").to_owned();
    let version = get_string(api().port_version);
    check("tsext_roundtrip", rest.starts_with("hello "), || format!("{rest:?}"));
    // The plugin API's `UNSUPPORTED` (-3): a bot isn't a port client.
    check("tsext_unsupported_bot", field("bot") == "-3", || format!("bot={}", field("bot")));
    let seen = with(|s| s.broadcast_seen);
    check("tsext_broadcast", field("all") == "1" && seen, || format!("all={} seen {seen}", field("all")));
    check("tsext_too_long", field("big") == INVALID.to_string(), || format!("big={}", field("big")));
    check("tsext_bad_channel", field("badch") == INVALID.to_string(), || format!("badch={}", field("badch")));
    check("client_port_version", field("ver") == version, || format!("ver={} (this client {version})", field("ver")));
}

/// Whether `data` holds `what`.
fn has(data: &[u8], what: &[u8]) -> bool {
    data.windows(what.len()).any(|w| w == what)
}

/// The interception checks: observe a state message, never see `TSExt`, `TSPkg` or `SrvSett`, block the
/// `SayText` with `tsext_block_me`, hold the one with `tsext_hold_me` (released from [`frame`]),
/// and rewrite every `DeathMsg`'s weapon as `tsext_rewritten` (the script's `kill`).
unsafe extern "C" fn message_pre(m: *const MessagePre, out: *mut u8, out_len: *mut c_int) -> c_int {
    if quiet() || m.is_null() {
        return verdict::PASS;
    }
    // SAFETY: the client passes a `MessagePre` whose name and data are valid for the call.
    let m = unsafe { *m };
    // SAFETY: as above.
    let (name, data) = unsafe {
        (CStr::from_ptr(m.name), if m.len <= 0 || m.data.is_null() { &[][..] } else { std::slice::from_raw_parts(m.data, m.len as usize) })
    };
    with(|s| {
        s.messages_seen += 1;
        if name == c"TSExt" || name == c"TSPkg" || name == c"SrvSett" {
            s.unreachable_seen += 1;
        }
    });
    if (name == c"TSHealth" || name == c"CurWeapon" || name == c"ScoreInfo") && m.allow == 0 && m.token == 0 {
        check("message_observe", true, String::new);
    }
    if name == c"MOTD" && with(|s| s.take_motd) && m.allow & allow::BLOCK != 0 {
        // A takeover (minor 2); every chunk of it.
        return verdict::BLOCK;
    }
    if name == c"SayText" && has(data, b"tsext_block_me") {
        check("message_block", m.allow & allow::BLOCK != 0, || format!("allow {}", m.allow));
        return verdict::BLOCK;
    }
    if name == c"SayText" && has(data, b"tsext_hold_me") && m.allow & allow::DEFER != 0 && m.token != 0 {
        with(|s| s.held = Some((m.token, 0)));
        return verdict::DEFER;
    }
    if name == c"DeathMsg" && m.allow & allow::REPLACE != 0 && data.len() >= 2 && !out.is_null() && !out_len.is_null() {
        let mut new = vec![data[0], data[1]];
        new.extend_from_slice(b"tsext_rewritten\0");
        // SAFETY: `out` has room for `MESSAGE_MAX` bytes (more than written) and `out_len` is
        // writable (the descriptor's contract).
        unsafe {
            std::ptr::copy_nonoverlapping(new.as_ptr(), out, new.len());
            *out_len = new.len() as c_int;
        }
        check("message_rewrite", true, String::new);
        return verdict::REPLACE;
    }
    verdict::PASS
}

/// The argv of a [`CommandPre`].
///
/// # Safety
/// `c` is valid for the call, as the client passes it.
unsafe fn words(c: &CommandPre) -> Vec<String> {
    (0..c.argc.max(0) as usize)
        // SAFETY: `argv` holds `argc` NUL-terminated strings (the caller's guarantee).
        .map(|i| unsafe { CStr::from_ptr(*c.argv.add(i)) }.to_string_lossy().into_owned())
        .collect()
}

unsafe extern "C" fn command_pre(c: *const CommandPre) -> c_int {
    if quiet() || c.is_null() {
        return verdict::PASS;
    }
    // SAFETY: the client passes a `CommandPre` valid for the call.
    let c = unsafe { *c };
    // SAFETY: as above.
    let argv = unsafe { words(&c) };
    let name = argv.first().cloned().unwrap_or_default();
    if (name == "+attack" || name == "+use") && c.allow == 0 {
        check("command_protected", true, String::new);
    }
    if name == "special" && with(|s| s.arm) == Arm::Block {
        check("command_block", c.allow & allow::BLOCK != 0, || format!("allow {}", c.allow));
        with(|s| s.special_blocked = true);
        return verdict::BLOCK;
    }
    verdict::PASS
}

unsafe extern "C" fn server_cmd_pre(c: *const CommandPre, out: *mut c_char, out_cap: c_int) -> c_int {
    if quiet() || c.is_null() {
        return verdict::PASS;
    }
    // SAFETY: the client passes a `CommandPre` valid for the call.
    let c = unsafe { *c };
    // SAFETY: as above.
    let argv = unsafe { words(&c) };
    if argv.first().map(String::as_str) != Some("_special") {
        return verdict::PASS;
    }
    match with(|s| s.arm) {
        Arm::Block => with(|s| s.special_sent_while_blocked += 1),
        Arm::Pass => {
            let (blocked, leaked) = with(|s| (s.special_blocked, s.special_sent_while_blocked));
            let can = c.allow & (allow::BLOCK | allow::REPLACE) == allow::BLOCK | allow::REPLACE;
            check("server_cmd_pre", can && blocked && leaked == 0, || format!("allow {} blocked {blocked} leaked {leaked}", c.allow));
        }
        Arm::Rename if !out.is_null() && out_cap > 5 => {
            // SAFETY: `out` has room for `out_cap` bytes (more than written).
            unsafe { std::ptr::copy_nonoverlapping(c"kill".as_ptr(), out, 5) };
            return verdict::REPLACE;
        }
        _ => {}
    }
    verdict::PASS
}

extern "C" fn vid_init() {
    if !quiet() {
        check("vid_init", true, String::new);
    }
}

extern "C" fn frame(_time: f64) {
    if quiet() {
        return;
    }
    let n = with(|s| {
        s.frames += 1;
        s.frames
    });
    check("frame", n > 10, || format!("{n} frames"));
    // The held `SayText` goes out 20 frames later; a second release finds nothing.
    let due = with(|s| {
        let (token, frames) = s.held.as_mut()?;
        *frames += 1;
        let t = (*frames > 20).then_some(*token)?;
        s.held = None;
        Some(t)
    });
    if let Some(t) = due {
        let (first, again) = (release(t, verdict::PASS), release(t, verdict::PASS));
        check("message_hold", first == OK, || format!("{first}"));
        check("release_again", again == ts_client_ext_api::NOT_FOUND, || format!("{again}"));
    }
    // A player killed in view is out of `visible_players` while it dies (minor 2).
    let look = with(|s| {
        let (slot, frames) = s.death_watch.as_mut()?;
        *frames += 1;
        let slot = (*frames == 10).then_some(*slot)?;
        s.death_watch = None;
        Some(slot)
    });
    if let Some(slot) = look {
        let mut vis = [VisiblePlayer::default(); 32];
        // SAFETY: a table function, called with room for `vis.len()` players.
        let n = unsafe { (api().visible_players)(vis.as_mut_ptr(), vis.len() as c_int) };
        let listed = vis[..(n.max(0) as usize).min(32)].iter().any(|v| v.slot == slot);
        check("visible_players_dead", !listed, || format!("player {slot} listed while dying"));
    }
}

extern "C" fn redraw(_time: f32, _intermission: c_int) {
    if quiet() {
        return;
    }
    let n = with(|s| {
        s.redraws += 1;
        s.redraws
    });
    check("redraw", n > 10, || format!("{n} redraws"));
    // A few redraws after hiding: the hidden health box still says where it would draw.
    let due = with(|s| {
        let n = s.hud_watch.as_mut()?;
        *n += 1;
        (*n > 5).then(|| s.hud_watch.take())
    });
    if due.is_some() {
        let list = hud_elements();
        let health = list.get(hud::HEALTH as usize).copied().unwrap_or_default();
        let want = hud_flag::HIDDEN | hud_flag::DRAWN | hud_flag::RECT;
        check("hud_hidden_kept", health.flags & want == want && health.rect[2] > 0 && health.rect[3] > 0, || format!("{health:?}"));
    }
}

unsafe extern "C" fn key_event(down: c_int, key: c_int, _binding: *const c_char) -> c_int {
    if quiet() || key != TEST_KEY {
        return 0;
    }
    let (d, u) = with(|s| {
        if down != 0 {
            s.key_down = true;
        } else {
            s.key_up = true;
        }
        (s.key_down, s.key_up)
    });
    check("key_event", d && u, || format!("down {d} up {u}"));
    1
}

extern "C" fn mouse_active(_active: c_int) {
    quiet();
}

extern "C" fn wants_cursor() -> c_int {
    if !quiet() {
        let n = with(|s| {
            s.cursor_polls += 1;
            s.cursor_polls
        });
        check("wants_cursor", n > 10, || format!("{n} polls"));
    }
    0
}

/// `+duck` held long enough for the duck to finish: the local player is ducking (minor 2).
fn note_duck(buttons: u32) {
    if buttons & button::DUCK == 0 {
        return;
    }
    let n = with(|s| {
        s.duck_cmds += 1;
        s.duck_cmds
    });
    // A duck takes 0.4 s (20 commands at 100 fps under xtask's 2x time scale); a little more.
    if n >= 30 {
        let mut ls = LocalState::default();
        // SAFETY: a table function, called with a valid out-pointer.
        let rc = unsafe { (api().local_state)(&mut ls) };
        check("local_ducking", rc == OK && ls.flags & local::DUCKING != 0, || format!("{rc} flags {:#x}", ls.flags));
    }
}

unsafe extern "C" fn usercmd(cmd: *const UserCmd) {
    if quiet() || cmd.is_null() {
        return;
    }
    // SAFETY: the client passes a `UserCmd` valid for the call (checked non-NULL).
    let c = unsafe { *cmd };
    let n = with(|s| {
        s.cmds += 1;
        s.cmds
    });
    check("usercmd", n > 10, || format!("{n} commands"));
    check("usercmd_attack", c.pressed & button::ATTACK != 0 && c.buttons & button::ATTACK != 0, || "no +attack edge".into());
    check("usercmd_use", c.pressed & button::USE != 0, || "no +use edge".into());
    check("usercmd_released", c.released & (button::ATTACK | button::USE) != 0, || "no release edge".into());
    note_duck(c.buttons);
}

unsafe extern "C" fn game_event(e: *const GameEvent) {
    if quiet() || e.is_null() {
        return;
    }
    // SAFETY: the client passes a `GameEvent` valid for the call (checked non-NULL).
    let e = unsafe { *e };
    let text = cstr_field(&e.text);
    match e.kind {
        event::MAP => check("event_map", text == "ts_dojo", || format!("map {text:?}")),
        event::SPAWN => check("event_spawn", true, String::new),
        event::HEALTH => check("event_health", e.a > 0, || format!("health {}", e.a)),
        event::WEAPON => check("event_weapon", e.a > 0 && !text.is_empty(), || format!("weapon {} {text:?}", e.a)),
        event::DEATH if e.c == 0 && e.b > 0 => with(|s| s.death_watch = Some((e.b, 0))),
        event::DEATH => check("event_death", e.c == 1 && e.b > 0, || format!("death {} {} {}", e.a, e.b, e.c)),
        _ => {}
    }
}

unsafe extern "C" fn suspend(_reason: *const c_char) {
    with(|s| s.suspended = true);
    check("suspend", true, String::new);
}

extern "C" fn resume() {
    let (was, n) = with(|s| (std::mem::replace(&mut s.suspended, false), s.while_suspended));
    check("resume", was, || "resume without suspend".into());
    check("suspended_quiet", was && n == 0, || format!("{n} callbacks while suspended"));
}

unsafe extern "C" fn server_changed(address: *const c_char) {
    if quiet() || address.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated string valid for the call (checked non-NULL).
    let a = unsafe { CStr::from_ptr(address) }.to_string_lossy().into_owned();
    if !a.is_empty() {
        print(&format!("server_changed {a}"));
    }
    check("server_changed", !a.is_empty(), || "only a disconnect".into());
}

extern "C" fn shutdown() {}

/// `tsext_test_spawn`: an unlisted file is refused, the listed helper starts.
extern "C" fn cmd_spawn() {
    let a = api();
    let me = id();
    // A file outside the approval (the game's own liblist.gam).
    let refused_path = CString::new("liblist.gam").unwrap_or_default();
    let helper = CString::new(format!("cl_dlls/{}", helper_name())).unwrap_or_default();
    let arg = c"from-tsmod_test";
    let argv = [arg.as_ptr()];
    // SAFETY: table functions, called with this extension's id, NUL-terminated paths and an
    // argument array of the length given.
    let (refused, started) = unsafe {
        ((a.verified_spawn)(me, refused_path.as_ptr(), 0, std::ptr::null()), (a.verified_spawn)(me, helper.as_ptr(), 1, argv.as_ptr()))
    };
    check("spawn_refused", refused == REFUSED, || format!("{refused}"));
    check("spawn_started", started > 0, || format!("{started}"));
}

/// The queries, against what the script set up: the host alive on ts_dojo with a stopped bot in
/// front of it.
fn queries() {
    let a = api();
    let mut ls = LocalState::default();
    // SAFETY: table functions, called with valid out-pointers, buffers and vectors (each of the
    // size the table's docs give) and NUL-terminated names. The same holds for every call below.
    let rc = unsafe { (a.local_state)(&mut ls) };
    check("local_state", rc == OK && ls.index > 0 && ls.alive == 1 && ls.health > 0, || format!("{rc} {ls:?}"));

    let mut row = PlayerScore::default();
    // SAFETY: as above.
    let rc = unsafe { (a.player)(ls.index, &mut row) };
    let name = cstr_field(&row.name);
    check("player", rc == OK && !name.is_empty() && row.flags & player::LOCAL != 0, || format!("{rc} {name:?} {:#x}", row.flags));

    let mut rows = [PlayerScore::default(); 32];
    // SAFETY: as above.
    let n = unsafe { (a.scores)(rows.as_mut_ptr(), rows.len() as c_int) };
    check("scores", n >= 2 && rows[..n as usize].iter().any(|r| r.flags & player::LOCAL != 0), || format!("{n} rows"));

    let mut aim = AimInfo::default();
    // SAFETY: as above.
    let rc = unsafe { (a.aim)(&mut aim) };
    check("aim", rc == OK && aim.hit != hit::NONE && aim.distance > 0.0, || format!("{rc} {aim:?}"));

    let mut tr = TraceInfo::default();
    let (start, end) = (ls.eye, [ls.eye[0], ls.eye[1], ls.eye[2] - 4096.0]);
    // SAFETY: as above.
    let rc = unsafe { (a.trace_line)(start.as_ptr(), end.as_ptr(), &mut tr) };
    check("trace_line", rc == OK && tr.fraction < 1.0 && tr.entity == 0, || format!("{rc} {tr:?}"));

    let mut vis = [VisiblePlayer::default(); 32];
    // SAFETY: as above.
    let n = unsafe { (a.visible_players)(vis.as_mut_ptr(), vis.len() as c_int) };
    check("visible_players", n >= 1 && vis[0].slot > 0 && vis[0].slot != ls.index && vis[0].distance > 0.0, || {
        format!("{n} {:?}", vis.first())
    });

    let mut v = ViewInfo::default();
    // SAFETY: as above.
    let rc = unsafe { (a.view)(&mut v) };
    let map = cstr_field(&v.map);
    check("view", rc == OK && v.width > 0 && v.height > 0 && v.fov > 0.0 && map == "ts_dojo", || {
        format!("{rc} {}x{} {} {map:?}", v.width, v.height, v.fov)
    });

    let ahead = [ls.eye[0] + ls.forward[0] * 100.0, ls.eye[1] + ls.forward[1] * 100.0, ls.eye[2] + ls.forward[2] * 100.0];
    let mut xy = [0f32; 2];
    // SAFETY: as above.
    let rc = unsafe { (a.world_to_screen)(ahead.as_ptr(), xy.as_mut_ptr()) };
    let inside = (0.0..v.width as f32).contains(&xy[0]) && (0.0..v.height as f32).contains(&xy[1]);
    check("world_to_screen", rc == OK && inside, || format!("{rc} {xy:?}"));

    let mut buf = [0 as c_char; NAME_LEN];
    // SAFETY: as above.
    unsafe { (a.weapon_name)(1, buf.as_mut_ptr(), buf.len() as c_int) };
    let w1 = cstr_field(&buf);
    // SAFETY: as above.
    unsafe { (a.weapon_name)(0, buf.as_mut_ptr(), buf.len() as c_int) };
    let w0 = cstr_field(&buf);
    check("weapon_name", !w1.is_empty() && w0 == "Kung Fu", || format!("{w1:?} {w0:?}"));

    // SAFETY: as above.
    let (before, set, after) = unsafe {
        let before = (a.cvar_value)(c"tsext_test_cvar".as_ptr());
        let set = (a.set_cvar_value)(c"tsext_test_cvar".as_ptr(), 7.0);
        (before, set, (a.cvar_value)(c"tsext_test_cvar".as_ptr()))
    };
    let mut sbuf = [0 as c_char; 16];
    // SAFETY: as above.
    unsafe { (a.cvar_string)(c"tsext_test_cvar".as_ptr(), sbuf.as_mut_ptr(), sbuf.len() as c_int) };
    let s = cstr_field(&sbuf);
    check("cvar", before == 5.0 && set == OK && after == 7.0 && s == "7", || format!("{before} {set} {after} {s:?}"));

    let (gd, pv, sa) = (get_string(a.game_dir), get_string(a.port_version), get_string(a.server_address));
    check("host_strings", gd == "ts" && !pv.is_empty() && !sa.is_empty(), || format!("{gd:?} {pv:?} {sa:?}"));

    let kind = (a.renderer_kind)();
    // SAFETY: as above.
    let p = unsafe { (a.gl_proc)(c"glGetString".as_ptr()) };
    let ok = match kind {
        renderer::SOFTWARE => p.is_null(),
        renderer::GL | renderer::GLES => !p.is_null(),
        _ => false,
    };
    check("renderer", ok, || format!("kind {kind} glGetString {p:?}"));

    // SAFETY: as above.
    let rc = unsafe { (a.client_cmd)(id(), c"echo [tsmod_test] client_cmd ran".as_ptr()) };
    check("client_cmd", rc == OK, || format!("{rc}"));

    let (mut w, mut h, mut x, mut y) = (0, 0, -1, -1);
    // SAFETY: as above.
    unsafe {
        (a.screen_size)(&mut w, &mut h);
        (a.mouse_position)(&mut x, &mut y);
    }
    check("mouse", w > 0 && h > 0 && x >= 0 && y >= 0, || format!("{w}x{h} at {x},{y}"));

    // Minor 2: every element, named; the health box (shown again) placed.
    let list = hud_elements();
    let named = list.iter().enumerate().all(|(i, e)| e.id == i as u32 && cstr_field(&e.name).as_bytes() == hud::NAMES[i].to_bytes());
    let health = list.get(hud::HEALTH as usize).copied().unwrap_or_default();
    let placed = health.flags & (hud_flag::RECT | hud_flag::HIDDEN) == hud_flag::RECT && health.rect[2] > 0;
    check("hud_elements", list.len() == hud::COUNT as usize && named && placed, || format!("{} named {named} {health:?}", list.len()));
}

/// The data directory: under `cl_plugins_data/test/`, with a trailing separator, writable.
fn data_dir() -> Option<String> {
    let mut buf = [0 as c_char; 1024];
    // SAFETY: a table function, called with this extension's id and a writable buffer.
    let rc = unsafe { (api().data_dir)(id(), buf.as_mut_ptr(), buf.len() as c_int) };
    // SAFETY: NUL-terminated by the table.
    let d = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    let sep = d.ends_with('/') || d.ends_with('\\');
    let under = d.replace('\\', "/").ends_with("/cl_plugins_data/test/");
    let writable = std::fs::write(format!("{d}probe.txt"), b"ok").is_ok();
    check("data_dir", rc > 0 && sep && under && writable, || format!("{rc} {d:?} writable {writable}"));
    (rc > 0).then_some(d)
}

/// `tsext_test_report`: runs the queries, then prints and writes every check.
extern "C" fn cmd_report() {
    if with(|s| s.suspended) {
        // The client must not run a suspended extension's commands.
        with(|s| s.while_suspended += 1);
        return;
    }
    queries();
    let (seen, bad) = with(|s| (s.messages_seen, s.unreachable_seen));
    check("message_unreachable", seen > 0 && bad == 0, || format!("{bad} of {seen} were TSExt or SrvSett"));
    let dir = data_dir();
    let lines: Vec<String> = with(|s| {
        CHECKS
            .iter()
            .map(|c| match s.checks.get(c) {
                Some((true, _)) => format!("check {c} ok"),
                Some((false, why)) => format!("check {c} FAIL {why}"),
                None => format!("check {c} FAIL not reached"),
            })
            .collect()
    });
    for l in &lines {
        print(l);
    }
    if let Some(d) = dir {
        let _ = std::fs::write(format!("{d}report.txt"), lines.join("\n") + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_are_unique() {
        let mut c = CHECKS.to_vec();
        c.sort();
        c.dedup();
        assert_eq!(c.len(), CHECKS.len());
    }
}
