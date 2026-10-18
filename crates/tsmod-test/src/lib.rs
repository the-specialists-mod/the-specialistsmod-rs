//! `tsmod_test`: the stub `tsmod_*` plugin (issue #104; docs/implementation/bot-plugins.md §6).
//! It registers a bot backend named after itself (`ts_bot_backend test`) whose bots walk in
//! circles and jump, and it calls the plugin API once each way, logging `[tsmod_test] check <what>
//! ok` lines that `cargo xtask plugins` looks for.
//!
//! Loaded under other names it exercises the loader's refusals: `tsmod_test_badver` reports
//! another API version, `tsmod_test_badsize` a wrong descriptor size, and `tsmod_test_refuse`
//! returns an error (after registering a command and a backend, which the server then drops).
//!
//! Plugin API version 5 (#198): it registers a listener (and removes it once heard), its bots
//! dive with the stunt key (`player_stunt`), use a power-up (`powerup 5` with `sv_cheats`, then
//! `usepowerup`: `player_powerup`), punch with kung fu while they have no gun (`melee_attack`,
//! whose answers alternate between a longer delay and a block, each checked by when the next
//! attack comes), one bot is killed by another through `Ham_TakeDamage` (`kill_scored`), and
//! `private_size` is asked about a bot.
//!
//! Plugin API version 6: its bots run with the server's view handling and `msec`
//! (`cmd::SET_VIEW | cmd::AUTO_MSEC`) and check what `run_cmd` reports (the spawn's `fixangle`,
//! landings, the respawned victim's teleport, the view kept, the game time put back). Every frame
//! it reads `players`, `frame_info` and `items` and checks them against `player_info`,
//! `player_inventory` (the inventory serial) and the engine. Once the kung-fu checks are done each
//! bot gets an M4 (`ts_give`), fires it (`weapon_fired`) and is handed a Beretta from a
//! `ts_groundweapon` through `Ham_Use` (`item_pickup`). It hears touches (`entity_touch`), the
//! scripted kill's `damage` event and sounds with their channel and responsible player, and reads
//! a non-entvars key back with `entity_key`.
//!
//! Plugin API version 7: at `game_init` it does what an AMX Mod X module does at `Meta_Attach`
//! (`attach_plugin` as `test_attached`: the name rules, its data directory, a command, a bot
//! backend and a listener owned by it, then `detach_plugin` and a second attach with a new id).
//! Every frame its bots' kung-fu and superjump flags must match `player_info`, and `frame_info`'s
//! team scores `game_state`'s.
//!
//! Plugin API version 8: `ext_message`, `client_port_version`, `offer_client_plugin` and
//! `client_plugin_version` on a bot (not a port client) and with bad arguments. On a listen server
//! with the port client (`cargo xtask client-plugins`) it answers the stub client extension's
//! `tsext_echo` command on the `TSExt` channel `test`. With the port client on a dedicated server
//! (`cargo xtask tspkg`) it offers on the client's `tsmod_test_offer <id>` (`offer_client_plugin`)
//! and logs every `client_plugin_state` and `client_plugin_list` event, with what
//! `client_plugin_version` says then.
//!
//! The plugin keeps its state behind a mutex, which it never holds while calling the API: the
//! server calls back into the plugin from inside API calls (`create_bot` spawns the bot).

#![warn(clippy::undocumented_unsafe_blocks)]
#![allow(clippy::missing_safety_doc)] // the only export is the plugin entry point, called by the server

use goldsrc_sys::{edict_t, enginefuncs_t, globalvars_t};
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
use ts_plugin_api::{
    BotBackend, BotCmd, BotCmdResult, ClientPlugin, DEFERRED, DamageEvent, EXISTS, EntityInfo, FrameInfo, GameState, HamArg, HamCall,
    INVALID, ItemInfo, KillInfo, LoadInfo, NO_PLUGIN, NOT_PLAYER, OK, ObjectiveInfo, PlayerInfo, PlayerSnapshot, Plugin, PluginApi,
    PluginId, UNSUPPORTED, VERSION, WeaponEntry, cmd, cmd_event, entity, ext_flag, ham, item, log, player_flag, powerup, stunt,
};

/// TS weapon id of the M4 (`shared::weapons::id::M4`), bought for the test bots, and of the
/// Beretta (`BERETTA`), handed over from a `ts_groundweapon`.
const M4: c_int = 5;
const BERETTA: c_int = 2;
/// hamsandwich's `Ham_Use` and the engine's `USE_TOGGLE`.
const HAM_USE: c_int = 43;
const USE_TOGGLE: c_int = 3;
/// `IN_JUMP`.
const IN_JUMP: c_int = goldsrc_sys::IN_JUMP as c_int;
/// hamsandwich's `Ham_Item_PreFrame`: the gun's `ItemPreFrame`, every frame a living player's
/// `m_flNextAttack` has run out.
const HAM_ITEM_PREFRAME: c_int = 70;
/// hamsandwich's `Ham_TakeDamage`: the scripted kill.
const HAM_TAKEDAMAGE: c_int = 9;
/// `IN_ATTACK` (a punch with kung fu) and `IN_ALT1` (the stunt key: a dive while running).
const IN_ATTACK: c_int = goldsrc_sys::IN_ATTACK as c_int;
const IN_ALT1: c_int = goldsrc_sys::IN_ALT1 as c_int;
/// The `melee_attack` answers: a changed move's delay (every kung-fu move's own is 1.35 s at
/// most, a held punch's 0.65) and a blocked one's. The next attack may not come sooner than
/// [`MELEE_SLACK`] of it (the bots' usercmd `msec` is rounded).
const MELEE_CHANGED: f32 = 2.0;
const MELEE_BLOCKED: f32 = 1.5;
const MELEE_SLACK: f32 = 0.8;
/// The game time after the backend's start at which one bot kills another.
const KILL_AFTER: f32 = 12.0;

#[derive(Clone)]
struct Bot {
    idx: c_int,
    /// Where it was when first seen alive this life, and the farthest it got from there.
    start: Option<[f32; 3]>,
    moved: f32,
    yaw: f32,
    next_jump: f32,
    next_respawn: f32,
    /// The checks run once per bot (API calls on a bot of ours).
    probed: bool,
    next_stunt: f32,
    /// When it was first seen alive this life (0: dead).
    alive_since: f32,
    /// The last `melee_attack` answer this life, and how many were asked.
    melee: Option<Melee>,
    melee_calls: u32,
    /// Version 6: the gun phase (an M4 given, the ground Beretta and when to use it), the next
    /// trigger press, and the last inventory serial with the list it stood for.
    armed: bool,
    ground: Option<(c_int, f32)>,
    next_fire: f32,
    serial: Option<(c_int, Vec<WeaponEntry>)>,
}

impl Bot {
    fn new(idx: c_int, yaw: f32) -> Bot {
        Bot {
            idx,
            start: None,
            moved: 0.0,
            yaw,
            next_jump: 0.0,
            next_respawn: 0.0,
            probed: false,
            next_stunt: 0.0,
            alive_since: 0.0,
            melee: None,
            melee_calls: 0,
            armed: false,
            ground: None,
            next_fire: 0.0,
            serial: None,
        }
    }
}

/// A `melee_attack` answer: when, and the delay it left.
#[derive(Clone, Copy)]
struct Melee {
    at: f32,
    delay: f32,
    blocked: bool,
}

/// The scripted kill: the bots, and whether `kill_scored` came.
#[derive(Clone, Copy)]
struct Kill {
    killer: c_int,
    victim: c_int,
}

struct State {
    id: PluginId,
    name: String,
    active: bool,
    added: u32,
    bots: Vec<Bot>,
    last_time: f32,
    next_report: f32,
    /// The `check` lines already logged.
    done: Vec<&'static str>,
    /// When the backend started (game time), and the scripted kill once made.
    active_since: f32,
    kill: Option<Kill>,
    /// The listener's handle (0: none), and whether it was heard and removed.
    listener: c_int,
    listener_heard: bool,
    listener_removed: bool,
}

/// The server's table, from `TSRS_PluginLoad`.
static API: AtomicPtr<PluginApi> = AtomicPtr::new(std::ptr::null_mut());

static STATE: Mutex<State> = Mutex::new(State {
    id: 0,
    name: String::new(),
    active: false,
    added: 0,
    bots: Vec::new(),
    last_time: 0.0,
    next_report: 0.0,
    done: Vec::new(),
    active_since: 0.0,
    kill: None,
    listener: 0,
    listener_heard: false,
    listener_removed: false,
});

/// Runs `f` on the state, or not at all when it is busy further up the stack.
fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.try_lock().ok().map(|mut s| f(&mut s))
}

fn api() -> Option<&'static PluginApi> {
    let p = API.load(Ordering::Relaxed);
    // SAFETY: a non-NULL pointer is the server's static table, which lives for the process.
    (!p.is_null()).then(|| unsafe { &*p })
}

fn id() -> PluginId {
    with(|s| s.id).unwrap_or(0)
}

fn say(level: c_int, msg: &str) {
    let (Some(a), Ok(m)) = (api(), CString::new(msg)) else { return };
    // SAFETY: `log` is the server's; the id is ours and `m` a NUL-terminated string for the call.
    unsafe { (a.log)(id(), level, m.as_ptr()) };
}

/// Logs `check <what> ok` (or FAIL) once per name.
fn check(what: &'static str, ok: bool) {
    let first = with(|s| {
        if s.done.contains(&what) {
            false
        } else {
            s.done.push(what);
            true
        }
    });
    if first == Some(true) || !ok {
        say(if ok { log::INFO } else { log::ERROR }, &format!("check {what} {}", if ok { "ok" } else { "FAIL" }));
    }
}

/// The server's engine table and globals, from the API.
fn engine() -> Option<(&'static enginefuncs_t, &'static globalvars_t)> {
    let a = api()?;
    let (f, g) = (a.engine_funcs.cast::<enginefuncs_t>(), a.globals.cast::<globalvars_t>().cast_const());
    // SAFETY: the API hands out the engine's table and globals, which live for the process; the
    // plugin only reads them, on the engine's thread.
    (!f.is_null() && !g.is_null()).then(|| unsafe { (&*f, &*g) })
}

/// The origin of entity `idx`, through the engine's own functions.
fn origin(idx: c_int) -> Option<[f32; 3]> {
    let (f, _) = engine()?;
    // SAFETY: `pfnPEntityOfEntIndex` takes any index and returns NULL or a live edict; its
    // `entvars_t` is the engine's, read here and not kept.
    unsafe {
        let e: *mut edict_t = (f.pfnPEntityOfEntIndex?)(idx);
        (!e.is_null()).then(|| (*e).v.origin)
    }
}

fn info(idx: c_int) -> Option<PlayerInfo> {
    let a = api()?;
    let mut i = PlayerInfo::default();
    // SAFETY: `i` is a `PlayerInfo` valid for writes.
    (unsafe { (a.player_info)(idx, &mut i) } == OK).then_some(i)
}

// ---- the entry point -----------------------------------------------------------------------------

/// `TSRS_PluginLoad`: checks the server's table, registers the backend and a status command, and
/// fills the descriptor (or misbehaves on purpose under the test names, module docs).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn TSRS_PluginLoad(
    api: *const PluginApi,
    api_size: u32,
    info: *const LoadInfo,
    out: *mut Plugin,
    out_size: u32,
) -> c_int {
    if api.is_null() || info.is_null() || out.is_null() || (out_size as usize) < size_of::<Plugin>() {
        return INVALID;
    }
    // SAFETY: the server passes its static table, valid for the process.
    let a: &'static PluginApi = unsafe { &*api };
    if (api_size as usize) < size_of::<PluginApi>() || a.version != VERSION {
        return INVALID;
    }
    // SAFETY: `info` and its strings live for the call.
    let (pid, name) = unsafe { ((*info).id, CStr::from_ptr((*info).name).to_string_lossy().into_owned()) };
    API.store(api.cast_mut(), Ordering::Relaxed);
    with(|s| {
        s.id = pid;
        s.name = name.clone();
    });
    if let Ok(cname) = CString::new(name.as_str()) {
        // SAFETY: the server copies the descriptor and reads the name during the call.
        let rc = unsafe { (a.register_bot_backend)(pid, cname.as_ptr(), &BACKEND) };
        say(log::INFO, &format!("loaded as plugin {pid}; bot backend \"{name}\": {rc}"));
    }
    let cmd = CString::new(format!("tsmod_{name}_status")).unwrap_or_default();
    // SAFETY: as above; `status` lives for the process.
    unsafe { (a.register_command)(pid, cmd.as_ptr(), status) };
    let mut desc = DESCRIPTOR;
    match name.as_str() {
        "test_refuse" => return INVALID,
        "test_badver" => desc.api_version = VERSION + 1,
        "test_badsize" => desc.size = 8,
        _ => {}
    }
    // SAFETY: `out` is a `Plugin` the server owns, writable (size checked above).
    unsafe { out.write(desc) };
    OK
}

const DESCRIPTOR: Plugin = Plugin {
    name: c"tsmod_test".as_ptr(),
    version: c"1".as_ptr(),
    game_init: Some(game_init),
    server_activate: Some(server_activate),
    start_frame: Some(start_frame),
    client_command: Some(client_command),
    player_spawned: Some(player_spawned),
    player_killed: Some(player_killed),
    sound: Some(sound),
    weapon_changed: Some(weapon_changed),
    damage: Some(damage),
    entity_touch: Some(entity_touch),
    weapon_fired: Some(weapon_fired),
    item_pickup: Some(item_pickup),
    player_stunt: Some(player_stunt),
    player_powerup: Some(player_powerup),
    melee_attack: Some(melee_attack),
    kill_scored: Some(kill_scored),
    client_plugin_state: Some(client_plugin_state),
    client_plugin_list: Some(client_plugin_list),
    ..Plugin::EMPTY
};

/// The listener (version 5): what a metamod plugin registers with `add_listener`.
const LISTENER: Plugin = Plugin { name: c"tsmod_test listener".as_ptr(), player_spawned: Some(listener_spawned), ..Plugin::EMPTY };
/// Version 7: the attached identity's listener.
const LISTENER_ATTACHED: Plugin =
    Plugin { name: c"tsmod_test attached listener".as_ptr(), player_spawned: Some(attached_spawned), ..Plugin::EMPTY };

const BACKEND: BotBackend = BotBackend {
    size: size_of::<BotBackend>() as u32,
    add_bot: Some(add_bot),
    activate: Some(activate),
    deactivate: Some(deactivate),
    adopt_bot: Some(adopt_bot),
};

// ---- plugin callbacks ------------------------------------------------------------------------------

extern "C" fn game_init() {
    let Some(a) = api() else { return };
    let mut buf = [0 as c_char; 512];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { (a.data_dir)(id(), buf.as_mut_ptr(), buf.len() as c_int) };
    // SAFETY: `data_dir` wrote a NUL-terminated string into `buf`.
    let dir = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
    let name = with(|s| s.name.clone()).unwrap_or_default();
    let tail = |sep: char| format!("tsmod{sep}{name}{sep}");
    check("data_dir", n > 0 && (dir.ends_with(&tail('/')) || dir.ends_with(&tail('\\'))));
    // The gun's entity hooks: `Ham_Item_PreFrame` on our bots' `weapon_tsgun`.
    (a.ham_set_hook)(Some(ham_hook));
    let rc = (a.ham_watch)(HAM_ITEM_PREFRAME, 1);
    say(log::INFO, &format!("ham_watch(Item_PreFrame): {rc}"));
    // A listener, and one the server must refuse (a short descriptor).
    let bad = Plugin { size: 8, ..LISTENER };
    // SAFETY: both descriptors live for the calls; the server copies the good one.
    let (handle, refused) = unsafe { ((a.add_listener)(NO_PLUGIN, &LISTENER), (a.add_listener)(NO_PLUGIN, &bad)) };
    with(|s| s.listener = handle);
    check("add_listener", handle > 0 && refused == INVALID);
    attach_checks(a, &name);
}

/// `data_dir` of `plugin`: its length (or status) and the directory.
fn data_dir_of(a: &PluginApi, plugin: PluginId) -> (c_int, String) {
    let mut buf = [0 as c_char; 512];
    // SAFETY: `buf` is writable for its length.
    let n = unsafe { (a.data_dir)(plugin, buf.as_mut_ptr(), buf.len() as c_int) };
    // SAFETY: `buf` holds a NUL-terminated string (zeroed when `data_dir` wrote nothing).
    (n, unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

/// Version 7: what an AMX Mod X module does with `attach_plugin` / `detach_plugin`.
fn attach_checks(a: &PluginApi, own: &str) {
    const ATTACHED: &CStr = c"test_attached";
    let own = CString::new(own).unwrap_or_default();
    // SAFETY: NULL or NUL-terminated names for the calls.
    let (id, again, loaded, bad, null) = unsafe {
        (
            (a.attach_plugin)(ATTACHED.as_ptr()),
            (a.attach_plugin)(c"TEST_Attached".as_ptr()),
            (a.attach_plugin)(own.as_ptr()),
            (a.attach_plugin)(c"a b".as_ptr()),
            (a.attach_plugin)(std::ptr::null()),
        )
    };
    let (n, dir) = data_dir_of(a, id);
    let in_dir = n > 0 && (dir.ends_with("tsmod/test_attached/") || dir.ends_with("tsmod\\test_attached\\"));
    // SAFETY: NUL-terminated names and descriptors that live for the calls; `status` lives for
    // the process, and the server copies the backend and the listener.
    let (cmd, backend, listener, stranger) = unsafe {
        (
            (a.register_command)(id, c"tsmod_test_attached_status".as_ptr(), status),
            (a.register_bot_backend)(id, ATTACHED.as_ptr(), &BACKEND),
            (a.add_listener)(id, &LISTENER_ATTACHED),
            (a.add_listener)(id + 1000, &LISTENER_ATTACHED),
        )
    };
    // SAFETY: a NUL-terminated message for the call.
    unsafe { (a.log)(id, log::INFO, c"attached".as_ptr()) };
    check(
        "attach_plugin",
        id > 0
            && id != with(|s| s.id).unwrap_or(0)
            && again == EXISTS
            && loaded == EXISTS
            && (bad, null) == (INVALID, INVALID)
            && in_dir
            && cmd == OK
            && backend == OK
            && listener > 0
            && stranger == INVALID,
    );
    let (first, second, of_loaded) = ((a.detach_plugin)(id), (a.detach_plugin)(id), (a.detach_plugin)(with(|s| s.id).unwrap_or(0)));
    let (gone_dir, _) = data_dir_of(a, id);
    // SAFETY: as above.
    let late_backend = unsafe { (a.register_bot_backend)(id, ATTACHED.as_ptr(), &BACKEND) };
    // SAFETY: as above.
    let new_id = unsafe { (a.attach_plugin)(ATTACHED.as_ptr()) };
    check(
        "detach_plugin",
        first == OK
            && second == INVALID
            && of_loaded == INVALID
            && (a.detach_plugin)(NO_PLUGIN) == INVALID
            && gone_dir == INVALID
            && late_backend == INVALID
            && (a.remove_listener)(listener) == INVALID
            && new_id > id
            && (a.detach_plugin)(new_id) == OK,
    );
}

/// The attached identity's listener: removed by `detach_plugin` before anything calls it.
extern "C" fn attached_spawned(_idx: c_int) {
    check("detach_plugin_listener", false);
}

/// The listener's `player_spawned`: heard once, and never after [`remove_listener`](PluginApi::remove_listener).
extern "C" fn listener_spawned(idx: c_int) {
    if !ours(idx) {
        return;
    }
    let removed = with(|s| {
        s.listener_heard = true;
        s.listener_removed
    });
    check("listener", true);
    if removed == Some(true) {
        check("remove_listener", false);
    }
}

/// The entity hook: `Item_PreFrame` must come on a `weapon_tsgun` (class `weapon_tsgun`, kind
/// `GUN`) held by one of our bots.
unsafe extern "C" fn ham_hook(call: *mut HamCall, post: c_int) -> c_int {
    let Some(a) = api() else { return ham::action::CONTINUE };
    // SAFETY: the server passes a `HamCall` valid for the call.
    let (ham, this, class) = unsafe { ((*call).ham, (*call).this, (*call).class) };
    if ham != HAM_ITEM_PREFRAME || post == 0 {
        return ham::action::CONTINUE;
    }
    let mut e = EntityInfo::default();
    // SAFETY: `e` is writable.
    let rc = unsafe { (a.entity_info)(this, &mut e) };
    let ours = with(|s| s.bots.iter().any(|b| b.idx == e.owner)).unwrap_or(false);
    // SAFETY: a NUL-terminated name for the call.
    let gun_class = unsafe { (a.class_id)(c"weapon_tsgun".as_ptr()) };
    if ours {
        check("gun_hook", rc == OK && e.kind == entity::GUN && class == gun_class && (a.ham_supported)(ham, class) == 1);
    }
    ham::action::CONTINUE
}

unsafe extern "C" fn server_activate(map: *const c_char) {
    let Some(a) = api() else { return };
    let mut buf = [0 as c_char; 64];
    // SAFETY: `buf` is writable for its length; `map` is the server's string for the call.
    let ok = unsafe { (a.map_name)(buf.as_mut_ptr(), buf.len() as c_int) > 0 && CStr::from_ptr(buf.as_ptr()) == CStr::from_ptr(map) };
    check("map_name", ok);
}

/// Every frame: each of our bots walks forward, turning, and jumps now and then; a dead one asks
/// to respawn. Every 3 s a line per bot.
extern "C" fn start_frame() {
    let Some((a, (_, g))) = api().zip(engine()) else { return };
    let now = g.time;
    let Some((active, bots, dt, report)) = with(|s| {
        let dt = (now - s.last_time).clamp(0.001, 0.1);
        s.last_time = now;
        let report = now >= s.next_report;
        if report {
            s.next_report = now + 3.0;
        }
        if s.active && s.active_since == 0.0 {
            s.active_since = now;
        }
        (s.active, s.bots.clone(), dt, report)
    }) else {
        return;
    };
    if !active {
        return;
    }
    let snaps = frame_checks(a, g);
    let mut kept = Vec::new();
    for mut b in bots {
        let Some(i) = info(b.idx) else { continue };
        if let Some(snap) = snaps.get(b.idx as usize - 1) {
            snapshot_checks(a, &mut b, snap, &i, now);
        }
        if b.probed || probe(a, b.idx, &i) {
            b.probed = true;
        }
        if i.alive == 0 {
            b.start = None;
            b.alive_since = 0.0;
            if now >= b.next_respawn {
                b.next_respawn = now + 1.0;
                // SAFETY: a NUL-terminated command for the call.
                unsafe { (a.bot_command)(b.idx, c"respawn".as_ptr()) };
            }
            kept.push(b);
            continue;
        }
        if let Some(o) = origin(b.idx) {
            let s = *b.start.get_or_insert(o);
            let d = ((o[0] - s[0]).powi(2) + (o[1] - s[1]).powi(2)).sqrt();
            b.moved = b.moved.max(d);
            if b.moved > 64.0 {
                check("moved", true);
            }
        }
        if b.alive_since == 0.0 {
            b.alive_since = now;
        }
        b.yaw = (b.yaw + 45.0 * dt) % 360.0;
        let jump = now >= b.next_jump;
        if jump {
            b.next_jump = now + 1.5;
        }
        // The stunt key now and then (never with jump, which would try a wall climb instead); a
        // punch held while it has kung fu out.
        let dive = !jump && now >= b.next_stunt;
        if dive {
            b.next_stunt = now + 2.5;
        }
        // A gun's trigger: a fresh press every second (semi-automatic fire needs one).
        let fire = i.weapon != 0 && now >= b.next_fire;
        if fire {
            b.next_fire = now + 1.0;
        }
        let attack = i.weapon == 0 || fire;
        let buttons = if jump { IN_JUMP } else { 0 } | if dive { IN_ALT1 } else { 0 } | if attack { IN_ATTACK } else { 0 };
        // Version 6: the server turns the bot and picks `msec` (from the frame time, carried).
        let cmd = BotCmd {
            angles: [0.0, b.yaw, 0.0],
            forward: 250.0,
            buttons,
            msec: 0,
            flags: cmd::SET_VIEW | cmd::AUTO_MSEC,
            ..BotCmd::default()
        };
        let mut r = BotCmdResult::default();
        // SAFETY: `cmd` and `r` are valid for the call.
        let rc = unsafe { (a.run_cmd)(b.idx, &cmd, &mut r) };
        check("run_cmd", rc == OK);
        // SAFETY: the engine's globals, read after the call that ran the bot's command (it may have
        // changed them; a volatile read doesn't assume it didn't).
        let after = unsafe { std::ptr::read_volatile(&g.time) };
        if rc == OK {
            result_checks(&mut b, &cmd, &r, now, after);
        }
        gun_phase(a, &mut b, now);
        if report {
            say(log::INFO, &format!("bot #{} weapon={} moved={:.0} alive={}", b.idx, i.weapon, b.moved, i.alive));
        }
        kept.push(b);
    }
    with(|s| {
        // Bots added during the loop (none in practice) are kept too, and the melee answers given
        // during it (the callback wrote them to the state).
        let extra: Vec<Bot> = s.bots.iter().filter(|x| !kept.iter().any(|k| k.idx == x.idx)).cloned().collect();
        for k in kept.iter_mut() {
            if let Some(b) = s.bots.iter().find(|b| b.idx == k.idx) {
                (k.melee, k.melee_calls) = (b.melee, b.melee_calls);
            }
        }
        s.bots = kept.into_iter().chain(extra).collect();
    });
    remove_heard_listener(a);
    scripted_kill(a, now);
}

/// Version 6, once a frame: `players` (every slot, the frame's), `frame_info` against the engine,
/// and `items` against `entity_info`. Returns the snapshots, slot 1 first.
fn frame_checks(a: &PluginApi, g: &globalvars_t) -> Vec<PlayerSnapshot> {
    let mut snaps = vec![PlayerSnapshot::default(); 64];
    // SAFETY: `snaps` is writable for 64 entries; a NULL buffer only counts.
    let (n, counted) = unsafe { ((a.players)(snaps.as_mut_ptr(), snaps.len() as c_int), (a.players)(std::ptr::null_mut(), 0)) };
    check("players_count", n == g.maxClients && counted == n);
    snaps.truncate(n.clamp(0, 64) as usize);
    let mut f = FrameInfo::default();
    let mut s = GameState::default();
    // SAFETY: `f` and `s` are writable; NULL is refused.
    let (rc, rc_null, rc_s) = unsafe { ((a.frame_info)(&mut f), (a.frame_info)(std::ptr::null_mut()), (a.game_state)(&mut s)) };
    check(
        "frame_info",
        rc == OK
            && rc_null == INVALID
            && rc_s == OK
            && f.time == g.time
            && f.highest_edict >= g.maxClients
            && (f.teamplay, f.ctb, f.mode, f.round) == (s.teamplay, s.ctb, s.mode, s.round)
            && f.team_scores == s.team_scores,
    );
    let mut items = vec![ItemInfo::default(); 256];
    // SAFETY: `items` is writable for 256 entries.
    let n = unsafe { (a.items)(items.as_mut_ptr(), items.len() as c_int) };
    items.truncate(n.clamp(0, 256) as usize);
    let consistent = items.iter().all(|it| {
        let mut e = EntityInfo::default();
        // SAFETY: `e` is writable.
        let rc = unsafe { (a.entity_info)(it.index, &mut e) };
        let kind = [entity::GROUND_WEAPON, entity::POWERUP, entity::GUN].contains(&it.kind);
        kind && it.index > g.maxClients && it.index <= f.highest_edict && rc == OK && e.kind == it.kind && e.weapon == it.weapon
    });
    check("items", n >= 0 && consistent);
    snaps
}

/// Version 6, per bot before its command: its slot in `players` agrees with `player_info` and the
/// engine; the inventory serial moves exactly when `player_inventory` changes; the combat timers
/// show a kung-fu move's delay.
fn snapshot_checks(a: &PluginApi, b: &mut Bot, snap: &PlayerSnapshot, i: &PlayerInfo, now: f32) {
    // What other bots' commands earlier in this frame can't have changed since the snapshot (a
    // punch can kill or disarm; only the bot's own command moves it).
    let alive = snap.flags & player_flag::ALIVE != 0;
    let same =
        snap.user_id > 0 && snap.flags & player_flag::BOT != 0 && snap.team == i.team && origin(b.idx).is_some_and(|o| o == snap.origin);
    check("players", same);
    // Version 7: the permanent power-ups, as `player_info` has them.
    let powers = |f: c_int| snap.flags & f != 0;
    check("snapshot_powers", powers(player_flag::KUNG_FU) == (i.kungfu != 0) && powers(player_flag::SUPERJUMP) == (i.superjump != 0));
    let mut inv = vec![WeaponEntry::default(); 40];
    // SAFETY: `inv` is writable for 40 entries.
    let n = unsafe { (a.player_inventory)(b.idx, inv.as_mut_ptr(), inv.len() as c_int) };
    inv.truncate(n.clamp(0, 40) as usize);
    // Only a moved serial with a changed list counts: other bots' moves earlier in this frame
    // (a kung-fu disarm) can change the list after the frame's snapshot was taken.
    if let Some((s, before)) = &b.serial
        && *s != snap.inventory_serial
        && *before != inv
    {
        check("inventory_serial", true);
    }
    b.serial = Some((snap.inventory_serial, inv));
    let c = &snap.combat;
    if alive {
        check("combat_power", (0.0..=100.0).contains(&c.melee_power));
    }
    // A move answered in the last half second (delay 1.5 or 2 s) keeps the next attack ahead.
    if let Some(m) = b.melee
        && now - m.at < 0.5
        && alive
    {
        check("combat", c.next_attack_at > now);
    }
}

/// Version 6: what `run_cmd` reported. The first command after a spawn carries the spawn's
/// `fixangle` (the bot adopts the game's view); afterwards the view is the command's; landings
/// come with the jumps; the respawned victim of the scripted kill was teleported; the game time is
/// the frame's again.
fn result_checks(b: &mut Bot, cmd: &BotCmd, r: &BotCmdResult, before: f32, after: f32) {
    check("run_cmd_time", after == before);
    check("run_cmd_result", (1..=255).contains(&r.msec) && origin(b.idx).is_some_and(|o| o == r.origin));
    if r.events & cmd_event::FIXANGLE != 0 {
        check("cmd_fixangle", true);
        b.yaw = r.v_angle[1].rem_euclid(360.0);
    } else {
        let d = (r.v_angle[1] - cmd.angles[1]).rem_euclid(360.0);
        // The stunts may hold the view (`FREEZE_ANGLES`), so only a match counts.
        if d.min(360.0 - d) < 0.01 {
            check("set_view", true);
        }
    }
    if r.events & cmd_event::LANDED != 0 {
        check("cmd_landed", true);
    }
    if r.events & cmd_event::TELEPORTED != 0 {
        check("cmd_teleported", true);
    }
}

/// Version 6: once the kung-fu checks are done, each bot gets an M4 (`ts_give`, a cheat the
/// server config allows) and a `ts_groundweapon` Beretta beside it, which it is handed 2 s later
/// (`Ham_Use` with the bot as activator, as RCBot's `givetsweapon`): `item_pickup`.
fn gun_phase(a: &PluginApi, b: &mut Bot, now: f32) {
    let melee_done = with(|s| s.done.contains(&"melee_attack_change") && s.done.contains(&"melee_attack_block")).unwrap_or(false);
    if !b.armed && melee_done {
        b.armed = true;
        // SAFETY: a NUL-terminated command for the call.
        unsafe { (a.bot_command)(b.idx, c"ts_give 5".as_ptr()) };
        (a.select_weapon)(b.idx, M4);
        b.ground = ground_weapon(a, b.idx, BERETTA).map(|e| (e, now + 2.0));
    }
    if let Some((e, at)) = b.ground
        && now >= at
    {
        b.ground = None;
        let ent = |i| HamArg { kind: ham::kind::ENTITY, i, ..HamArg::default() };
        let mut call = HamCall { ham: HAM_USE, this: e, argc: 4, ..HamCall::default() };
        call.args[..4].copy_from_slice(&[
            ent(b.idx),
            ent(b.idx),
            HamArg { kind: ham::kind::INT, i: USE_TOGGLE, ..HamArg::default() },
            HamArg { kind: ham::kind::FLOAT, f: 0.0, ..HamArg::default() },
        ]);
        // SAFETY: `call` is a `HamCall` valid for the call.
        let rc = unsafe { (a.ham_execute)(&mut call, 1) };
        say(log::INFO, &format!("bot #{} uses the ground Beretta #{e}: {rc}", b.idx));
        remove_entity(e);
    }
}

/// A `ts_groundweapon` of weapon `id` at the bot's origin, from keys (the map's way).
fn ground_weapon(a: &PluginApi, bot: c_int, id: c_int) -> Option<c_int> {
    let o = origin(bot)?;
    let id = CString::new(id.to_string()).ok()?;
    let at = CString::new(format!("{} {} {}", o[0], o[1], o[2])).ok()?;
    // SAFETY: NUL-terminated strings for the calls.
    unsafe {
        let e = (a.create_entity)(c"ts_groundweapon".as_ptr());
        if e <= 0 {
            return None;
        }
        (a.entity_key_value)(e, c"tsweaponid".as_ptr(), id.as_ptr());
        (a.entity_key_value)(e, c"origin".as_ptr(), at.as_ptr());
        ((a.spawn_entity)(e) == OK).then_some(e)
    }
}

/// Frees entity `e` with the engine's `pfnRemoveEntity`.
fn remove_entity(e: c_int) {
    if let Some((f, _)) = engine()
        && let (Some(get), Some(remove)) = (f.pfnPEntityOfEntIndex, f.pfnRemoveEntity)
    {
        // SAFETY: the engine's functions; `get` returns NULL or a live edict.
        unsafe {
            let ed = get(e);
            if !ed.is_null() && (*ed).free == 0 {
                remove(ed);
            }
        }
    }
}

/// Removes the listener once it was heard: the handle goes, a second removal is refused.
fn remove_heard_listener(a: &PluginApi) {
    let Some(handle) = with(|s| (s.listener_heard && !s.listener_removed && s.listener > 0).then_some(s.listener)).flatten() else {
        return;
    };
    let (first, second) = ((a.remove_listener)(handle), (a.remove_listener)(handle));
    with(|s| s.listener_removed = true);
    check("remove_listener", first == OK && second == INVALID);
}

/// Once, [`KILL_AFTER`] s into the backend's run: a bot that has been alive for 3 s kills
/// another through `Ham_TakeDamage` (1000 damage, the killer its own inflictor), which the rules
/// score and report (`kill_scored`).
fn scripted_kill(a: &PluginApi, now: f32) {
    let pick = with(|s| {
        if s.kill.is_some() || s.active_since == 0.0 || now < s.active_since + KILL_AFTER {
            return None;
        }
        let ready: Vec<c_int> =
            s.bots.iter().filter(|b| b.probed && b.alive_since > 0.0 && now >= b.alive_since + 3.0).map(|b| b.idx).collect();
        let k = Kill { killer: *ready.first()?, victim: *ready.get(1)? };
        s.kill = Some(k);
        Some(k)
    })
    .flatten();
    let Some(k) = pick else { return };
    let ent = |i| HamArg { kind: ham::kind::ENTITY, i, ..HamArg::default() };
    let mut call = HamCall { ham: HAM_TAKEDAMAGE, this: k.victim, argc: 4, ..HamCall::default() };
    call.args[..4].copy_from_slice(&[
        ent(k.killer),
        ent(k.killer),
        HamArg { kind: ham::kind::FLOAT, f: 1000.0, ..HamArg::default() },
        HamArg { kind: ham::kind::INT, i: 0, ..HamArg::default() },
    ]);
    // SAFETY: `call` is a `HamCall` valid for reads and writes during the call.
    let rc = unsafe { (a.ham_execute)(&mut call, 1) };
    say(log::INFO, &format!("bot #{} kills bot #{} (Ham_TakeDamage): {rc}", k.killer, k.victim));
    check("ham_take_damage", rc == OK);
}

/// The API calls on one of our living bots, once: true when done.
fn probe(a: &PluginApi, idx: c_int, i: &PlayerInfo) -> bool {
    if i.alive == 0 {
        return false;
    }
    check("player_info", i.is_bot == 1);
    let mut e = EntityInfo::default();
    // SAFETY: `e` is writable.
    let rc = unsafe { (a.entity_info)(idx, &mut e) };
    check("entity_info", rc == OK && e.kind == entity::PLAYER);
    let mut g = GameState::default();
    // SAFETY: `g` is writable.
    let rc = unsafe { (a.game_state)(&mut g) };
    check("game_state", rc == OK && g.max_clients > 0 && g.time > 0.0);
    check("team_scores", g.team_scores.iter().all(|&t| t >= 0));
    let mut objs = [ObjectiveInfo::default(); 8];
    // SAFETY: `objs` is writable for 8 entries; a NULL buffer only counts.
    let (n_bot, n_none, bad) = unsafe {
        (
            (a.objectives)(idx, objs.as_mut_ptr(), objs.len() as c_int),
            (a.objectives)(0, std::ptr::null_mut(), 0),
            (a.objectives)(-1, std::ptr::null_mut(), 0),
        )
    };
    check("objectives", n_bot >= 0 && n_bot == n_none && bad == NOT_PLAYER);
    check("create_entity", create_entity(a));
    let mut inv = [WeaponEntry::default(); 40];
    // SAFETY: `inv` is writable for 40 entries.
    let n = unsafe { (a.player_inventory)(idx, inv.as_mut_ptr(), inv.len() as c_int) };
    check("inventory", n >= 0);
    // Through the client command path, to our own `client_command`.
    // SAFETY: a NUL-terminated command for the call.
    let rc = unsafe { (a.bot_command)(idx, c"tsmod_test_ping 1".as_ptr()) };
    check("bot_command", rc == OK || rc == DEFERRED);
    check("private_size", (a.private_size)(idx) > 0 && (a.private_size)(-1) == NOT_PLAYER);
    check("ext_message", ext_message_checks(a, idx));
    check("client_port_version", client_port_version_checks(a, idx));
    check("offer_client_plugin", offer_client_plugin_checks(a, idx));
    check("client_plugin_version", client_plugin_version_checks(a, idx));
    // A held double fire rate (the cheat; the server config turns `sv_cheats` on), then used:
    // `player_powerup` with its bit.
    // SAFETY: NUL-terminated commands for the calls.
    unsafe {
        (a.bot_command)(idx, c"powerup 5".as_ptr());
        (a.bot_command)(idx, c"usepowerup".as_ptr());
    }
    true
}

/// `PluginApi::ext_message` (version 8) on a bot-only server: a bot isn't a port client, nobody
/// gets a broadcast, and the arguments are checked.
fn ext_message_checks(a: &PluginApi, bot: c_int) -> bool {
    let fits = [7u8; ts_plugin_api::EXT_MESSAGE_MAX - 5];
    // SAFETY: NUL-terminated channels and payloads of the lengths given (NULL with 0).
    unsafe {
        let to_bot = (a.ext_message)(bot, c"test".as_ptr(), b"x".as_ptr(), 1, 0);
        let all = (a.ext_message)(0, c"test".as_ptr(), std::ptr::null(), 0, ext_flag::UNRELIABLE);
        let nobody = (a.ext_message)(-1, c"test".as_ptr(), std::ptr::null(), 0, 0);
        let too_long = (a.ext_message)(0, c"test".as_ptr(), fits.as_ptr(), fits.len() as c_int + 1, 0);
        let longest = (a.ext_message)(0, c"test".as_ptr(), fits.as_ptr(), fits.len() as c_int, 0);
        let bad_channel = (a.ext_message)(0, c"a b".as_ptr(), std::ptr::null(), 0, 0);
        let bad_flag = (a.ext_message)(0, c"test".as_ptr(), std::ptr::null(), 0, 2);
        let null_data = (a.ext_message)(0, c"test".as_ptr(), std::ptr::null(), 3, 0);
        let ok = to_bot == UNSUPPORTED
            && all == 0
            && nobody == NOT_PLAYER
            && too_long == INVALID
            && longest == 0
            && bad_channel == INVALID
            && bad_flag == INVALID
            && null_data == INVALID;
        if !ok {
            say(log::ERROR, &format!("ext_message: {to_bot} {all} {nobody} {too_long} {longest} {bad_channel} {bad_flag} {null_data}"));
        }
        ok
    }
}

/// `PluginApi::client_port_version` (version 8): a bot has no port version; slot 0 isn't a player.
fn client_port_version_checks(a: &PluginApi, bot: c_int) -> bool {
    let mut buf = [b'x' as c_char; 16];
    // SAFETY: `buf` is writable for its length.
    let (n, none) =
        unsafe { ((a.client_port_version)(bot, buf.as_mut_ptr(), buf.len() as c_int), (a.client_port_version)(0, buf.as_mut_ptr(), 0)) };
    n == 0 && buf[0] == 0 && none == NOT_PLAYER
}

/// `PluginApi::offer_client_plugin` (version 8) on a server without offers: every id is unknown
/// (`INVALID`, which comes before the bot's `UNSUPPORTED`), and slot -1 isn't a player.
fn offer_client_plugin_checks(a: &PluginApi, bot: c_int) -> bool {
    // SAFETY: NUL-terminated ids, or NULL.
    let (unknown, null, all, nobody) = unsafe {
        (
            (a.offer_client_plugin)(bot, c"tsmod_test_none".as_ptr()),
            (a.offer_client_plugin)(bot, std::ptr::null()),
            (a.offer_client_plugin)(0, c"tsmod_test_none".as_ptr()),
            (a.offer_client_plugin)(-1, c"tsmod_test_none".as_ptr()),
        )
    };
    let ok = unknown == INVALID && null == INVALID && all == INVALID && nobody == NOT_PLAYER;
    if !ok {
        say(log::ERROR, &format!("offer_client_plugin: {unknown} {null} {all} {nobody}"));
    }
    ok
}

/// `PluginApi::client_plugin_version` (version 8): a bot runs no client plugins (`UNSUPPORTED`),
/// NULL is `INVALID`, slot 0 isn't a player; the buffer isn't touched for any of them.
fn client_plugin_version_checks(a: &PluginApi, bot: c_int) -> bool {
    let mut buf = [b'x' as c_char; 16];
    // SAFETY: NUL-terminated ids or NULL; `buf` is writable for its length.
    let (on_bot, null, nobody) = unsafe {
        (
            (a.client_plugin_version)(bot, c"webview".as_ptr(), buf.as_mut_ptr(), buf.len() as c_int),
            (a.client_plugin_version)(bot, std::ptr::null(), buf.as_mut_ptr(), buf.len() as c_int),
            (a.client_plugin_version)(0, c"webview".as_ptr(), buf.as_mut_ptr(), buf.len() as c_int),
        )
    };
    let ok = on_bot == UNSUPPORTED && null == INVALID && nobody == NOT_PLAYER && buf[0] == b'x' as c_char;
    if !ok {
        say(log::ERROR, &format!("client_plugin_version: {on_bot} {null} {nobody}"));
    }
    ok
}

/// What `client_plugin_version` says about `id` for `player`: the version, `-` for none, or the
/// status in parentheses.
fn loaded_version(a: &PluginApi, player: c_int, id: &CStr) -> String {
    let mut buf = [0 as c_char; 32];
    // SAFETY: a NUL-terminated id and a writable buffer.
    let n = unsafe { (a.client_plugin_version)(player, id.as_ptr(), buf.as_mut_ptr(), buf.len() as c_int) };
    match n {
        0 => "-".into(),
        // SAFETY: the server NUL-terminated `buf`.
        n if n > 0 => unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned(),
        code => format!("({code})"),
    }
}

/// `tsmod_test_offer <id>` from a port client (`cargo xtask tspkg`): offers `id` through the API,
/// and an id nobody offers. Logs `offer_client_plugin #<player> <id>: <sent> unknown=<rc>`.
fn offer_command(a: &PluginApi, player: c_int, id: &CStr) {
    // SAFETY: NUL-terminated ids for the calls.
    let (sent, unknown) =
        unsafe { ((a.offer_client_plugin)(player, id.as_ptr()), (a.offer_client_plugin)(player, c"tsmod_test_none".as_ptr())) };
    say(log::INFO, &format!("offer_client_plugin #{player} {}: {sent} unknown={unknown}", id.to_string_lossy()));
}

/// `ts_plugin_api::plugin_state`'s names (the `tspkg` words), by value.
const STATE_NAMES: [&str; 11] =
    ["have", "offered", "downloading", "installed", "loaded", "denied", "blocked", "unsupported", "failed", "suspended", "restart"];

/// A state for the log: its `tspkg` word, else the number.
fn state_name(state: c_int) -> String {
    usize::try_from(state).ok().and_then(|i| STATE_NAMES.get(i)).map_or_else(|| state.to_string(), |n| (*n).to_owned())
}

/// Version 8: a client's `tspkg` answer. Logs `client_plugin_state #<player> <id> <version>
/// <state> [<reason>] loaded=<client_plugin_version>`.
unsafe extern "C" fn client_plugin_state(player: c_int, id: *const c_char, version: *const c_char, state: c_int, reason: *const c_char) {
    let (Some(a), false) = (api(), id.is_null() || version.is_null() || reason.is_null()) else { return };
    // SAFETY: the server passes NUL-terminated strings that live for the call.
    let (id, version, reason) = unsafe { (CStr::from_ptr(id), CStr::from_ptr(version), CStr::from_ptr(reason)) };
    let reason = if reason.is_empty() { String::new() } else { format!(" [{}]", reason.to_string_lossy()) };
    let loaded = loaded_version(a, player, id);
    let (id, version) = (id.to_string_lossy(), version.to_string_lossy());
    say(log::INFO, &format!("client_plugin_state #{player} {id} {version} {}{reason} loaded={loaded}", state_name(state)));
}

/// Version 8: a client's `tspkg list` is complete. Logs `client_plugin_list #<player>: <n>
/// [<id>@<version> ...] versions agree|differ`: whether `client_plugin_version` agrees with every
/// entry.
unsafe extern "C" fn client_plugin_list(player: c_int, plugins: *const ClientPlugin, count: c_int) {
    let Some(a) = api() else { return };
    let entries: &[ClientPlugin] = match usize::try_from(count) {
        // SAFETY: the server passes `count` entries that live for the call.
        Ok(n) if n > 0 && !plugins.is_null() => unsafe { std::slice::from_raw_parts(plugins, n) },
        _ => &[],
    };
    let mut listed = Vec::new();
    let mut agree = true;
    for p in entries.iter().filter(|p| !p.id.is_null() && !p.version.is_null()) {
        // SAFETY: as above: NUL-terminated strings for the call.
        let (id, version) = unsafe { (CStr::from_ptr(p.id), CStr::from_ptr(p.version)) };
        agree &= loaded_version(a, player, id) == version.to_string_lossy();
        listed.push(format!("{}@{}", id.to_string_lossy(), version.to_string_lossy()));
    }
    let versions = if agree { "agree" } else { "differ" };
    say(log::INFO, &format!("client_plugin_list #{player}: {count} [{}] versions {versions}", listed.join(" ")));
}

/// `tsext_echo <text>` from a port client's extension (the stub client extension's
/// `tsext_test_tsext`, `cargo xtask client-plugins`): answers on its `TSExt` channel `test` with
/// the text and what `ext_message` and `client_port_version` said about the bot, a broadcast, a
/// payload too long and a bad channel, and the client's version. Also sends on a channel nobody
/// claimed, which the client must drop.
fn tsext_echo(a: &PluginApi, player: c_int, text: &str) {
    let max = engine().map_or(0, |(_, g)| g.maxClients);
    let bot = (1..=max).find(|&i| info(i).is_some_and(|p| p.is_bot == 1));
    let big = [0u8; ts_plugin_api::EXT_MESSAGE_MAX];
    let mut ver = [0 as c_char; 32];
    // SAFETY: NUL-terminated channels, payloads of the lengths given, a writable buffer.
    let reply = unsafe {
        let bot_rc = bot.map_or(0, |b| (a.ext_message)(b, c"test".as_ptr(), b"x".as_ptr(), 1, 0));
        let all = (a.ext_message)(0, c"test".as_ptr(), b"broadcast".as_ptr(), 9, 0);
        let too_long = (a.ext_message)(player, c"test".as_ptr(), big.as_ptr(), big.len() as c_int, 0);
        let bad_channel = (a.ext_message)(player, c"no;way".as_ptr(), b"x".as_ptr(), 1, 0);
        (a.ext_message)(player, c"nochan".as_ptr(), b"dropped".as_ptr(), 7, 0);
        (a.client_port_version)(player, ver.as_mut_ptr(), ver.len() as c_int);
        let ver = CStr::from_ptr(ver.as_ptr()).to_string_lossy().into_owned();
        format!("echo:{text} bot={bot_rc} all={all} big={too_long} badch={bad_channel} ver={ver}")
    };
    // SAFETY: as above.
    let rc = unsafe { (a.ext_message)(player, c"test".as_ptr(), reply.as_ptr(), reply.len() as c_int, 0) };
    say(log::INFO, &format!("tsext_echo to #{player}: {reply} ({rc})"));
}

/// Creates an `info_target` from keys (as RCBot's `givetsweapon` makes a `ts_groundweapon`),
/// spawns it, checks it, and removes it again. An unknown class is refused.
fn create_entity(a: &PluginApi) -> bool {
    // SAFETY: NUL-terminated strings for the API calls; `i` is writable; the engine's
    // `pfnPEntityOfEntIndex` returns the live edict we created, which `pfnRemoveEntity` frees.
    unsafe {
        if (a.create_entity)(c"tsmod_test_no_such_class".as_ptr()) != INVALID {
            return false;
        }
        let e = (a.create_entity)(c"info_target".as_ptr());
        if e <= 0 {
            return false;
        }
        let kv = (a.entity_key_value)(e, c"targetname".as_ptr(), c"tsmod_test_target".as_ptr());
        let placed = (a.entity_key_value)(e, c"origin".as_ptr(), c"1 2 3".as_ptr());
        let spawned = (a.spawn_entity)(e);
        let mut i = EntityInfo::default();
        let ok = kv == 1 && placed == 1 && spawned == OK && (a.entity_info)(e, &mut i) == OK && origin(e) == Some([1.0, 2.0, 3.0]);
        // Version 6: a key the entvars table doesn't have stays readable; an entvars one isn't
        // kept (read the edict).
        (a.entity_key_value)(e, c"master".as_ptr(), c"tsmod_test_ms".as_ptr());
        let mut buf = [0 as c_char; 32];
        let n = (a.entity_key)(e, c"master".as_ptr(), buf.as_mut_ptr(), buf.len() as c_int);
        let master = n == 13 && CStr::from_ptr(buf.as_ptr()) == c"tsmod_test_ms";
        let others = (a.entity_key)(e, c"targetname".as_ptr(), buf.as_mut_ptr(), buf.len() as c_int) == UNSUPPORTED
            && (a.entity_key)(-1, c"master".as_ptr(), buf.as_mut_ptr(), buf.len() as c_int) == NOT_PLAYER
            && (a.entity_key)(e, std::ptr::null(), buf.as_mut_ptr(), buf.len() as c_int) == INVALID;
        check("entity_key", master && others);
        if let Some((f, _)) = engine()
            && let (Some(get), Some(remove)) = (f.pfnPEntityOfEntIndex, f.pfnRemoveEntity)
        {
            remove(get(e));
        }
        ok
    }
}

unsafe extern "C" fn client_command(player: c_int, argc: c_int, argv: *const *const c_char) -> c_int {
    if argc < 1 || argv.is_null() {
        return 0;
    }
    // SAFETY: the server passes `argc` NUL-terminated strings for the call.
    let cmd = unsafe { CStr::from_ptr(*argv) };
    if cmd == c"tsext_echo" {
        // SAFETY: as above.
        let text = if argc > 1 { unsafe { CStr::from_ptr(*argv.add(1)) }.to_string_lossy().into_owned() } else { String::new() };
        if let Some(a) = api() {
            tsext_echo(a, player, &text);
        }
        return 1;
    }
    if cmd == c"tsmod_test_offer" {
        if let (Some(a), true) = (api(), argc > 1) {
            // SAFETY: as above.
            offer_command(a, player, unsafe { CStr::from_ptr(*argv.add(1)) });
        }
        return 1;
    }
    if cmd != c"tsmod_test_ping" {
        return 0;
    }
    check("client_command", argc == 2);
    1
}

fn ours(idx: c_int) -> bool {
    with(|s| s.bots.iter().any(|b| b.idx == idx)).unwrap_or(false)
}

/// A spawn of one of our bots: an M4 and kevlar for the next life, and the M4 in hand once it
/// has one.
extern "C" fn player_spawned(idx: c_int) {
    let Some(a) = api() else { return };
    if !ours(idx) {
        return;
    }
    // A new life: the gun's timers start over, so the melee answers do too.
    with(|s| {
        if let Some(b) = s.bots.iter_mut().find(|b| b.idx == idx) {
            (b.melee, b.melee_calls) = (None, 0);
        }
    });
    check("spawned", true);
    let rc = (a.buy)(idx, M4, 0, 0, 4);
    check("buy", rc == OK || rc == DEFERRED);
    let rc = (a.buy_item)(idx, item::KEVLAR);
    check("buy_item", rc == OK || rc == DEFERRED);
    let mut inv = [WeaponEntry::default(); 40];
    // SAFETY: `inv` is writable for 40 entries.
    let n = unsafe { (a.player_inventory)(idx, inv.as_mut_ptr(), inv.len() as c_int) };
    if inv.iter().take(n.max(0) as usize).any(|w| w.weapon == M4) {
        let rc = (a.select_weapon)(idx, M4);
        check("select_weapon", rc == OK || rc == DEFERRED);
    }
}

extern "C" fn player_killed(victim: c_int, _attacker: c_int, _inflictor: c_int) {
    if ours(victim) {
        check("killed", true);
    }
}

unsafe extern "C" fn sound(
    entity: c_int,
    origin: *const f32,
    sample: *const c_char,
    volume: f32,
    _attenuation: f32,
    channel: c_int,
    source_player: c_int,
) {
    check("sound", !origin.is_null() && !sample.is_null() && (0.0..=1.0).contains(&volume) && (0..=7).contains(&channel));
    // Version 6: a sound one of our bots made (footsteps, jumps, punches) names it; one of its
    // things (its gun entity, a thrown knife) names it as the owner.
    if ours(source_player) {
        let clients = engine().map_or(0, |(_, g)| g.maxClients);
        check("sound_source", source_player == entity || entity > clients);
    }
}

/// Version 6: the scripted kill's `Ham_TakeDamage` as a damage event; kung-fu hits on our bots
/// carry a hitgroup.
unsafe extern "C" fn damage(event: *const DamageEvent) {
    if event.is_null() {
        check("damage", false);
        return;
    }
    // SAFETY: the server passes a `DamageEvent` valid for the call.
    let d = unsafe { *event };
    if with(|s| s.kill.is_some_and(|k| k.victim == d.victim && k.killer == d.attacker)) == Some(true) && d.damage == 1000.0 {
        check("damage", d.inflictor == d.attacker && d.health <= 0.0 && d.weapon >= -1);
    }
    if ours(d.victim) && d.hitgroup > 0 {
        check("damage_hitgroup", d.hitgroup <= 7);
    }
}

/// Version 6: our bots touch the world, each other and triggers as they walk and jump.
extern "C" fn entity_touch(player: c_int, other: c_int) {
    if ours(player) {
        check("entity_touch", other >= 0 && other != player);
    }
}

/// Version 6: the M4 the gun phase gave a bot, fired.
unsafe extern "C" fn weapon_fired(player: c_int, weapon: c_int, at: *const f32, dir: *const f32, _silenced: c_int) {
    if !ours(player) {
        return;
    }
    if at.is_null() || dir.is_null() {
        check("weapon_fired", false);
        return;
    }
    // SAFETY: three floats each, valid for the call.
    let (at, dir) = unsafe { (*at.cast::<[f32; 3]>(), *dir.cast::<[f32; 3]>()) };
    let len = dir.iter().map(|x| x * x).sum::<f32>().sqrt();
    let near = origin(player).is_some_and(|o| o.iter().zip(at).all(|(a, b)| (a - b).abs() < 64.0));
    check("weapon_fired", weapon > 0 && (len - 1.0).abs() < 0.01 && near);
}

/// Version 6: the ground Beretta the gun phase handed a bot.
extern "C" fn item_pickup(player: c_int, _entity: c_int, kind: c_int, id: c_int) {
    if ours(player) && kind == entity::GROUND_WEAPON {
        check("item_pickup", id == BERETTA);
    }
}

extern "C" fn weapon_changed(idx: c_int, _weapon: c_int) {
    if ours(idx) {
        check("weapon_changed", true);
    }
}

/// Version 5: a stunt of one of our bots (they dive; the dive's roll or getting up follow).
extern "C" fn player_stunt(idx: c_int, kind: c_int) {
    if ours(idx) {
        check("player_stunt", (stunt::ROLL..=stunt::WALL_CLIMB).contains(&kind));
    }
}

/// Version 5: the double fire rate [`probe`] had a bot use.
extern "C" fn player_powerup(idx: c_int, bits: c_int) {
    if ours(idx) && bits == powerup::DFIRERATE {
        check("player_powerup", true);
    }
}

/// Version 5: a kung-fu move of one of our bots. The answers alternate: a changed move (delay
/// [`MELEE_CHANGED`], damage 1) and a blocked one (delay [`MELEE_BLOCKED`]); the next call of
/// the same life checks the delay held (`melee_attack_change` / `melee_attack_block`).
unsafe extern "C" fn melee_attack(idx: c_int, time: *mut f32, damage: *mut f32) -> c_int {
    let Some((_, g)) = engine() else { return 0 };
    if time.is_null() || damage.is_null() {
        check("melee_attack", false);
        return 0;
    }
    let now = g.time;
    // SAFETY: the server passes pointers valid for the call.
    let (t, d) = unsafe { (*time, *damage) };
    let Some((prev, n)) = with(|s| {
        s.bots.iter_mut().find(|b| b.idx == idx).map(|b| {
            b.melee_calls += 1;
            (b.melee, b.melee_calls)
        })
    })
    .flatten() else {
        return 0;
    };
    check("melee_attack", t.is_finite() && t > 0.0 && d.is_finite() && d >= 0.0);
    if let Some(m) = prev {
        check(if m.blocked { "melee_attack_block" } else { "melee_attack_change" }, now - m.at >= m.delay * MELEE_SLACK);
    }
    let blocked = n % 2 == 0;
    let (nt, nd) = if blocked { (MELEE_BLOCKED, d) } else { (MELEE_CHANGED, 1.0) };
    // SAFETY: as above.
    unsafe { (*time, *damage) = (nt, nd) };
    with(|s| {
        if let Some(b) = s.bots.iter_mut().find(|b| b.idx == idx) {
            b.melee = Some(Melee { at: now, delay: nt, blocked });
        }
    });
    blocked as c_int
}

/// Version 5: the scripted kill ([`scripted_kill`]) as the rules scored it: the killer, victim
/// and inflictor, points (TS 3.0: 1, 2 for a kung-fu frag, more for a stunt or a double kill), a
/// spree of at least 1, the weapon and its name.
unsafe extern "C" fn kill_scored(kill: *const KillInfo) {
    if kill.is_null() {
        check("kill_scored", false);
        return;
    }
    // SAFETY: the server passes a `KillInfo` valid for the call.
    let k = unsafe { *kill };
    if with(|s| s.kill.is_some_and(|x| x.killer == k.killer && x.victim == k.victim)) != Some(true) {
        return;
    }
    // SAFETY: `weapon_name` is NUL-terminated within its array (the server cuts it to fit).
    let name = unsafe { CStr::from_ptr(k.weapon_name.as_ptr()) };
    say(
        log::INFO,
        &format!(
            "kill_scored: killer {} victim {} inflictor {} points {} weapon {} \"{}\" tags {:#x} sprees {}/{}",
            k.killer,
            k.victim,
            k.inflictor,
            k.points,
            k.weapon,
            name.to_string_lossy(),
            k.tags,
            k.killer_spree,
            k.victim_spree
        ),
    );
    check(
        "kill_scored",
        k.inflictor == k.killer && k.points >= 1 && k.killer_spree >= 1 && k.victim_spree >= 0 && k.weapon >= 0 && !name.is_empty(),
    );
}

/// `tsmod_<name>_status`: our bots.
unsafe extern "C" fn status(_argc: c_int, _argv: *const *const c_char) {
    let bots = with(|s| s.bots.clone()).unwrap_or_default();
    say(log::INFO, &format!("{} bots", bots.len()));
    for b in bots {
        let i = info(b.idx).unwrap_or_default();
        say(log::INFO, &format!("  #{} alive={} weapon={} team={} moved={:.0}", b.idx, i.alive, i.weapon, i.team, b.moved));
    }
}

// ---- the bot backend -------------------------------------------------------------------------------

/// `addbot`, `addcustombot`, the quota: a bot named as asked, else "Test Bot <n>".
unsafe extern "C" fn add_bot(name: *const c_char, model: *const c_char, skill: f32) -> c_int {
    let Some(a) = api() else { return 0 };
    let n = with(|s| {
        s.added += 1;
        s.added
    })
    .unwrap_or(1);
    // SAFETY: the server passes NUL-terminated strings for the call.
    let given = unsafe { CStr::from_ptr(name) };
    // A given name goes back to `create_bot` as its bytes (the API's strings are bytes, #190).
    let cname = if given.is_empty() { CString::new(format!("Test Bot {n}")).unwrap_or_default() } else { given.to_owned() };
    let name = cname.to_string_lossy().into_owned();
    // SAFETY: as above; `create_bot` reads the strings during the call.
    let idx = unsafe { (a.create_bot)(id(), cname.as_ptr(), model, skill) };
    if idx > 0 {
        with(|s| s.bots.push(Bot::new(idx, (n * 97 % 360) as f32)));
        say(log::INFO, &format!("added \"{name}\" (#{idx})"));
        check("create_bot", true);
    } else {
        say(log::WARNING, &format!("create_bot \"{name}\": {idx}"));
    }
    idx
}

extern "C" fn activate() {
    with(|s| s.active = true);
    say(log::INFO, "bot backend active");
}

extern "C" fn deactivate() {
    with(|s| {
        s.active = false;
        s.active_since = 0.0;
        s.bots.clear();
    });
    say(log::INFO, "bot backend inactive");
}

extern "C" fn adopt_bot(idx: c_int) {
    with(|s| s.bots.push(Bot::new(idx, 0.0)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_matches_this_api() {
        assert_eq!(DESCRIPTOR.size as usize, size_of::<Plugin>());
        assert_eq!(DESCRIPTOR.api_version, VERSION);
        assert_eq!(BACKEND.size as usize, size_of::<BotBackend>());
        assert_eq!((LISTENER.size as usize, LISTENER.api_version), (size_of::<Plugin>(), VERSION));
        assert!(DESCRIPTOR.player_stunt.is_some() && DESCRIPTOR.player_powerup.is_some());
        assert!(DESCRIPTOR.melee_attack.is_some() && DESCRIPTOR.kill_scored.is_some());
        assert!(DESCRIPTOR.damage.is_some() && DESCRIPTOR.entity_touch.is_some());
        assert!(DESCRIPTOR.weapon_fired.is_some() && DESCRIPTOR.item_pickup.is_some());
        assert!(DESCRIPTOR.client_plugin_state.is_some() && DESCRIPTOR.client_plugin_list.is_some());
    }

    #[test]
    fn state_names_follow_the_api() {
        use ts_plugin_api::plugin_state::*;
        assert_eq!(state_name(HAVE), "have");
        assert_eq!(state_name(DENIED), "denied");
        assert_eq!(state_name(RESTART), "restart");
        assert_eq!(state_name(11), "11");
        assert_eq!(state_name(-1), "-1");
    }

    #[test]
    fn a_null_or_short_call_is_refused() {
        let mut out = Plugin::EMPTY;
        // SAFETY: NULL arguments are refused before anything is read.
        let rc = unsafe { TSRS_PluginLoad(std::ptr::null(), 0, std::ptr::null(), &mut out, size_of::<Plugin>() as u32) };
        assert_eq!(rc, INVALID);
    }
}
