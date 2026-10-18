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
//! Under any name starting with `quiet` it is a minimal extension for the approval tests (issue
//! #297): it loads, prints `quiet stub init` from `init`, and, installed as a package with a
//! helper, starts that helper through `verified_spawn`.

#![warn(clippy::undocumented_unsafe_blocks)]

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
use ts_client_ext_api::{
    AimInfo, ClientExt, ClientExtApi, EXISTS, GameEvent, INVALID, LoadInfo, LocalState, MINOR, NAME_LEN, OK, PlayerScore, REFUSED,
    TraceInfo, UserCmd, VERSION, ViewInfo, VisiblePlayer, button, event, hit, player, renderer,
};

/// The key `cargo xtask client-plugins` presses with `ts_dev_key` (`K_AUX32`, unbound): the stub
/// swallows it.
pub const TEST_KEY: c_int = 238;

/// The checks the stub reports, in order. xtask requires each to be `ok`.
pub const CHECKS: [&str; 41] = [
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
];

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
            *out = ClientExt { name: c"quiet client stub".as_ptr(), version: c"1".as_ptr(), init: Some(quiet_init), ..ClientExt::EMPTY };
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
    check("load", t.minor == MINOR && reserved_clear, || format!("minor {} reserved clear {reserved_clear}", t.minor));
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
