//! The plugin API table (`ts_plugin_api::PluginApi`) that `CreateInterface` and the `tsmod_*`
//! loader (`crate::plugins`) hand out: a port addition. Each function converts its arguments and
//! calls `ts_server::plugin_api`, `ts_server::plugins::host` or, for the entity hooks,
//! `ts_server::ham`, where the logic is (docs/implementation/plugin-api.md). Calling the plugin's
//! hook and command functions is FFI, so it is here too ([`call_hook`]).

use goldsrc::Global;
use goldsrc::sys::{KeyValueData, TraceResult};
use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::rc::Rc;
use std::sync::OnceLock;
use ts_plugin_api::{
    BotBackend, BotCmd, BotCmdResult, EntityInfo, FrameInfo, GameState, HamCall, INVALID, ItemInfo, NOT_PLAYER, OK, ObjectiveInfo,
    PlayerInfo, PlayerSnapshot, PluginApi, PluginId, VERSION, WeaponEntry, ham,
};
use ts_server::ham as hooks;
use ts_server::plugin_api as api;
use ts_server::plugins::host;

/// The table, built once the engine has given the game its tables (the plugins read the engine's
/// pointers from it).
struct Table(PluginApi);
// SAFETY: the table holds function pointers and the engine's table and globals pointers, which
// the engine owns for the process; plugins only read it, on the engine's thread.
unsafe impl Sync for Table {}
// SAFETY: as above; it never leaves that thread in practice, and nothing in it is thread-bound.
unsafe impl Send for Table {}

static TABLE: OnceLock<Table> = OnceLock::new();
/// The table for a `CreateInterface` before `GiveFnptrsToDll` (no engine pointers); never in
/// practice, as metamod and the engine give the game its tables first.
static EARLY: OnceLock<Table> = OnceLock::new();

/// The one table.
pub fn table() -> &'static PluginApi {
    match goldsrc::server::try_engine() {
        Some(e) => &TABLE.get_or_init(|| Table(build(e.raw_tables()))).0,
        None => &EARLY.get_or_init(|| Table(build((std::ptr::null(), std::ptr::null_mut())))).0,
    }
}

fn build((funcs, globals): (*const goldsrc::sys::enginefuncs_t, *mut goldsrc::sys::globalvars_t)) -> PluginApi {
    PluginApi {
        size: size_of::<PluginApi>() as u32,
        version: VERSION,
        engine_funcs: funcs.cast(),
        globals: globals.cast(),
        log,
        data_dir,
        map_name,
        register_command,
        register_cvar,
        register_bot_backend,
        player_info,
        player_inventory,
        set_cash,
        set_free_slots,
        set_award,
        run_powerup,
        set_speed,
        set_physics_speed,
        set_bullet_trail,
        set_weapon_ammo,
        projectile_owner,
        entity_info,
        master_triggered,
        game_state,
        objectives,
        create_entity,
        entity_key_value,
        spawn_entity,
        create_bot,
        kick_bot,
        run_cmd,
        bot_command,
        select_weapon,
        buy,
        buy_item,
        clear_loadout,
        drop_weapon,
        ham_set_hook,
        ham_watch,
        ham_supported,
        class_id,
        entity_class_id,
        ham_execute,
        add_listener,
        remove_listener,
        private_size,
        players,
        items,
        frame_info,
        entity_key,
        attach_plugin,
        detach_plugin,
        ext_message,
        client_port_version,
        offer_client_plugin,
        client_plugin_version,
    }
}

/// A caller's string argument: `None` for NULL.
///
/// # Safety
/// A non-NULL `p` is a NUL-terminated string that lives for the call (the table's contract).
unsafe fn arg<'a>(p: *const c_char) -> Option<&'a CStr> {
    // SAFETY: the caller's guarantee.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) })
}

/// Writes `s` into the caller's buffer, NUL-terminated and cut to fit, and returns its full
/// length. Guard: a NULL buffer or a length below 1 only gives the length.
///
/// # Safety
/// A non-NULL `buf` is writable for `len` bytes (the table's contract).
unsafe fn put_str(buf: *mut c_char, len: c_int, s: &str) -> c_int {
    // SAFETY: the caller's guarantee.
    unsafe { put_bytes(buf, len, s.as_bytes()) }
}

/// [`put_str`] for bytes that needn't be UTF-8.
///
/// # Safety
/// As [`put_str`].
unsafe fn put_bytes(buf: *mut c_char, len: c_int, s: &[u8]) -> c_int {
    if !buf.is_null() && len > 0 {
        let n = s.len().min(len as usize - 1);
        // SAFETY: `buf` is writable for `len` bytes and `n + 1 <= len`; `s` doesn't overlap it.
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), buf.cast::<u8>(), n);
            *buf.add(n) = 0;
        }
    }
    s.len().min(c_int::MAX as usize) as c_int
}

// ---- the host -------------------------------------------------------------------------------------

unsafe extern "C" fn log(plugin: PluginId, level: c_int, msg: *const c_char) {
    // SAFETY: the table's contract for string arguments.
    if let Some(m) = unsafe { arg(msg) } {
        host::log(plugin, level, &m.to_string_lossy());
    }
}

unsafe extern "C" fn data_dir(plugin: PluginId, buf: *mut c_char, len: c_int) -> c_int {
    match host::data_dir(plugin) {
        // SAFETY: the table's contract for buffers.
        Ok(d) => unsafe { put_str(buf, len, &d) },
        Err(e) => e,
    }
}

unsafe extern "C" fn map_name(buf: *mut c_char, len: c_int) -> c_int {
    let eng = goldsrc::server::engine();
    let name = eng.string(eng.globals().mapname).to_string_lossy().into_owned();
    // SAFETY: the table's contract for buffers.
    unsafe { put_str(buf, len, &name) }
}

unsafe extern "C" fn register_command(plugin: PluginId, name: *const c_char, f: ts_plugin_api::CommandFn) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(name) = (unsafe { arg(name) }).and_then(|n| n.to_str().ok()) else { return INVALID };
    let call = move |argv: &[&CStr]| {
        let ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        // SAFETY: `f` is the plugin's command function, callable on the engine's thread for the
        // process (libraries stay loaded); `ptrs` holds `argc` NUL-terminated strings that live
        // for the call.
        unsafe { f(ptrs.len() as c_int, ptrs.as_ptr()) }
    };
    host::register_command(plugin, name, Rc::new(call))
}

unsafe extern "C" fn register_cvar(_plugin: PluginId, name: *const c_char, value: *const c_char, flags: c_int) -> *mut c_void {
    // SAFETY: the table's contract for string arguments.
    let (Some(name), value) = (unsafe { arg(name) }, unsafe { arg(value) }) else { return std::ptr::null_mut() };
    let value = value.map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
    host::register_cvar(&name.to_string_lossy(), &value, flags).cast()
}

unsafe extern "C" fn register_bot_backend(plugin: PluginId, name: *const c_char, backend: *const BotBackend) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(name) = (unsafe { arg(name) }).and_then(|n| n.to_str().ok()) else { return INVALID };
    if backend.is_null() {
        return INVALID;
    }
    // SAFETY: a non-NULL `backend` points to a `BotBackend` the caller owns for the call; only
    // its `size` is read first, then as many bytes as both sides know.
    let size = unsafe { (&raw const (*backend).size).read_unaligned() } as usize;
    if size < size_of::<BotBackend>() {
        return INVALID;
    }
    // SAFETY: as above, `size_of::<BotBackend>()` bytes are readable.
    let b = unsafe { backend.read_unaligned() };
    crate::plugins::register_bot_backend(plugin, name, b)
}

// ---- players ------------------------------------------------------------------------------------

/// Guard: a NULL `out` gives `NOT_PLAYER` (the contract asks for a valid pointer); nothing is
/// written on failure.
unsafe extern "C" fn player_info(player: c_int, out: *mut PlayerInfo) -> c_int {
    if out.is_null() {
        return NOT_PLAYER;
    }
    match api::player_info(player) {
        Ok(info) => {
            // SAFETY: the caller passes a `PlayerInfo` valid for writes (the table's contract);
            // `write_unaligned` doesn't rely on its alignment.
            unsafe { out.write_unaligned(info) };
            OK
        }
        Err(code) => code,
    }
}

unsafe extern "C" fn player_inventory(player: c_int, out: *mut WeaponEntry, max: c_int) -> c_int {
    match api::player_inventory(player) {
        Ok(list) => {
            if !out.is_null() {
                for (i, w) in list.iter().take(max.max(0) as usize).enumerate() {
                    // SAFETY: the caller passes `out` writable for `max` entries (the table's
                    // contract), and `i < max`.
                    unsafe { out.add(i).write_unaligned(*w) };
                }
            }
            list.len() as c_int
        }
        Err(code) => code,
    }
}

extern "C" fn set_cash(player: c_int, cash: c_int) -> c_int {
    api::set_cash(player, cash)
}

extern "C" fn set_free_slots(player: c_int, slots: c_int) -> c_int {
    api::set_free_slots(player, slots)
}

extern "C" fn set_award(player: c_int, award: c_int) -> c_int {
    api::set_award(player, award)
}

extern "C" fn run_powerup(player: c_int, powerup: c_int, duration: f32) -> c_int {
    api::run_powerup(player, powerup, duration)
}

extern "C" fn set_speed(player: c_int, speed: f32, aura: f32, duration: f32) -> c_int {
    api::set_speed(player, speed, aura, duration)
}

extern "C" fn set_physics_speed(player: c_int, speed: f32) -> c_int {
    api::set_physics_speed(player, speed)
}

extern "C" fn set_bullet_trail(player: c_int, on: c_int) -> c_int {
    api::set_bullet_trail(player, on)
}

extern "C" fn set_weapon_ammo(player: c_int, weapon: c_int, clip: c_int, ammo: c_int) -> c_int {
    api::set_weapon_ammo(player, weapon, clip, ammo)
}

// ---- entities and the game ---------------------------------------------------------------------

extern "C" fn projectile_owner(entity: c_int) -> c_int {
    api::projectile_owner(entity)
}

unsafe extern "C" fn entity_info(entity: c_int, out: *mut EntityInfo) -> c_int {
    if out.is_null() {
        return INVALID;
    }
    match api::world_info::entity_info(entity) {
        Ok(info) => {
            // SAFETY: the caller passes an `EntityInfo` valid for writes (the table's contract).
            unsafe { out.write_unaligned(info) };
            OK
        }
        Err(code) => code,
    }
}

unsafe extern "C" fn master_triggered(master: *const c_char, activator: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments; NULL is no master.
    let name = unsafe { arg(master) }.unwrap_or(c"");
    api::world_info::master_triggered(name, activator)
}

unsafe extern "C" fn game_state(out: *mut GameState) -> c_int {
    if out.is_null() {
        return INVALID;
    }
    let s = api::world_info::game_state();
    // SAFETY: the caller passes a `GameState` valid for writes (the table's contract).
    unsafe { out.write_unaligned(s) };
    OK
}

unsafe extern "C" fn objectives(player: c_int, out: *mut ObjectiveInfo, max: c_int) -> c_int {
    match api::world_info::objectives(player) {
        Ok(list) => {
            if !out.is_null() {
                for (i, o) in list.iter().take(max.max(0) as usize).enumerate() {
                    // SAFETY: the caller passes `out` writable for `max` entries (the table's
                    // contract), and `i < max`.
                    unsafe { out.add(i).write_unaligned(*o) };
                }
            }
            list.len() as c_int
        }
        Err(code) => code,
    }
}

unsafe extern "C" fn create_entity(classname: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(class) = (unsafe { arg(classname) }) else { return INVALID };
    api::world_info::create_entity(class)
}

unsafe extern "C" fn entity_key_value(entity: c_int, key: *const c_char, value: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let (Some(key), Some(value)) = (unsafe { arg(key) }, unsafe { arg(value) }) else { return INVALID };
    api::world_info::entity_key_value(entity, key, value)
}

extern "C" fn spawn_entity(entity: c_int) -> c_int {
    api::world_info::spawn_entity(entity)
}

// ---- bots -----------------------------------------------------------------------------------------

unsafe extern "C" fn create_bot(plugin: PluginId, name: *const c_char, model: *const c_char, skill: f32) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let (Some(name), model) = (unsafe { arg(name) }, unsafe { arg(model) }) else { return INVALID };
    api::bots::create_bot(plugin, name, model.unwrap_or(c""), skill)
}

extern "C" fn kick_bot(player: c_int) -> c_int {
    api::bots::kick_bot(player)
}

/// Guard: a NULL `cmd` is `INVALID`; a NULL `out` is allowed (no result wanted).
unsafe extern "C" fn run_cmd(player: c_int, cmd: *const BotCmd, out: *mut BotCmdResult) -> c_int {
    if cmd.is_null() {
        return INVALID;
    }
    // SAFETY: the caller passes a `BotCmd` valid for reads (the table's contract).
    let c = unsafe { cmd.read_unaligned() };
    match api::bots::run_cmd(player, &c) {
        Ok(r) => {
            if !out.is_null() {
                // SAFETY: a non-NULL `out` is a `BotCmdResult` valid for writes (the contract).
                unsafe { out.write_unaligned(r) };
            }
            OK
        }
        Err(code) => code,
    }
}

unsafe extern "C" fn bot_command(player: c_int, line: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(line) = (unsafe { arg(line) }) else { return INVALID };
    api::bots::bot_command(player, &line.to_string_lossy())
}

extern "C" fn select_weapon(player: c_int, weapon: c_int) -> c_int {
    api::bots::select_weapon(player, weapon)
}

extern "C" fn buy(player: c_int, weapon: c_int, attachments: c_int, akimbo: c_int, ammo: c_int) -> c_int {
    api::bots::buy(player, weapon, attachments, akimbo, ammo)
}

extern "C" fn buy_item(player: c_int, item: c_int) -> c_int {
    api::bots::buy_item(player, item)
}

extern "C" fn clear_loadout(player: c_int) -> c_int {
    api::bots::clear_loadout(player)
}

extern "C" fn drop_weapon(player: c_int, weapon: c_int) -> c_int {
    api::bots::drop_weapon(player, weapon)
}

// ---- entity hooks ---------------------------------------------------------------------------------

/// The plugin's hook, as `ham_set_hook` installed it.
static HOOK: Global<Cell<Option<ham::Hook>>> = Global(Cell::new(None));

extern "C" fn ham_set_hook(hook: Option<ham::Hook>) {
    HOOK.0.set(hook);
    hooks::set_hook(hook.map(|_| call_hook as hooks::HookFn));
}

/// The plugin's hook as `ts_server::ham` calls it.
fn call_hook(call: &mut HamCall, post: bool) -> c_int {
    let Some(hook) = HOOK.0.get() else { return ham::action::CONTINUE };
    // SAFETY: `hook` is what the plugin installed with `ham_set_hook`, callable on the engine's
    // thread until it removes it (the table's contract); `call` is an exclusive `HamCall` that
    // lives for the call. The game holds no borrow of entity state across it (`ts_server::ham`'s
    // module docs), so the hook may call back into the table.
    unsafe { hook(call, post as c_int) }
}

extern "C" fn ham_watch(ham: c_int, on: c_int) -> c_int {
    hooks::watch(ham, on != 0)
}

extern "C" fn ham_supported(ham: c_int, class: c_int) -> c_int {
    hooks::supported(ham, class)
}

/// Guard: a NULL name is `INVALID`.
unsafe extern "C" fn class_id(classname: *const c_char) -> c_int {
    if classname.is_null() {
        return INVALID;
    }
    // SAFETY: a non-NULL name is the caller's NUL-terminated string, read for this call only.
    hooks::class_id(unsafe { CStr::from_ptr(classname) })
}

extern "C" fn entity_class_id(entity: c_int) -> c_int {
    hooks::entity_class_id(entity)
}

/// The call is copied in and only `ret` is written back, so a plugin passing the `HamCall` of the
/// hook it is in (or nesting another `ham_execute`) never aliases a live reference. The pointer
/// arguments are read here: a `TRACE` argument's `TraceResult` (copied in and back) and
/// `Ham_Keyvalue`'s `KeyValueData` (its strings read, `fHandled` written back). Guard: a NULL
/// `call` is `INVALID`, and so is a NULL `TraceResult` / `KeyValueData` / key / value.
unsafe extern "C" fn ham_execute(call: *mut HamCall, run_hooks: c_int) -> c_int {
    if call.is_null() {
        return INVALID;
    }
    // SAFETY: the caller passes a `HamCall` valid for reads and writes for this call (the table's
    // contract); `read_unaligned` doesn't rely on its alignment.
    let mut c = unsafe { call.read_unaligned() };
    let argc = usize::try_from(c.argc).unwrap_or(0).min(ham::MAX_ARGS);
    let trace_ptr =
        c.args[..argc].iter().find(|a| a.kind == ham::kind::TRACE).map(|a| a.p.cast_mut().cast::<TraceResult>()).filter(|p| !p.is_null());
    // SAFETY: a `TRACE` argument points to a `TraceResult` valid for the call (contract).
    let mut trace = trace_ptr.map(|p| unsafe { p.read_unaligned() });
    let kvd_ptr = (c.ham == hooks::id::KEYVALUE as c_int && argc >= 1)
        .then(|| c.args[0].p.cast_mut().cast::<KeyValueData>())
        .filter(|p| !p.is_null());
    // SAFETY: `Ham_Keyvalue`'s argument points to a `KeyValueData` valid for the call (contract).
    let kvd = kvd_ptr.map(|p| unsafe { p.read_unaligned() });
    let mut handled = kvd.map_or(0, |k| k.fHandled);
    let strings = kvd.and_then(|k| {
        let ok = !k.szKeyName.is_null() && !k.szValue.is_null();
        // SAFETY: its non-NULL strings are NUL-terminated and live for the call (contract).
        ok.then(|| unsafe {
            let class = (!k.szClassName.is_null()).then(|| CStr::from_ptr(k.szClassName));
            (class, CStr::from_ptr(k.szKeyName), CStr::from_ptr(k.szValue))
        })
    });
    let r = {
        let mut ptrs = hooks::Pointers {
            trace: trace.as_mut(),
            kvd: strings.map(|(class, key, value)| hooks::KeyValue { class, key, value, handled: &mut handled }),
        };
        hooks::execute(&mut c, &mut ptrs, run_hooks != 0)
    };
    if let (Some(p), Some(t)) = (trace_ptr, trace) {
        // SAFETY: as when it was read.
        unsafe { p.write_unaligned(t) };
    }
    if let Some(p) = kvd_ptr.filter(|_| strings.is_some()) {
        // SAFETY: as when it was read; only `fHandled` is written.
        unsafe { (&raw mut (*p).fHandled).write_unaligned(handled) };
    }
    // SAFETY: as when it was read.
    unsafe { (&raw mut (*call).ret).write_unaligned(c.ret) };
    r
}

// ---- version 5: listeners, private data --------------------------------------------------------

/// Guard: NULL is `INVALID`, and so is a descriptor whose `size` isn't this version's (only `size`
/// is read then).
unsafe extern "C" fn add_listener(plugin: PluginId, listener: *const ts_plugin_api::Plugin) -> c_int {
    if listener.is_null() {
        return INVALID;
    }
    // SAFETY: a non-NULL `listener` points to a `Plugin` the caller owns for the call; its `size`
    // (the first field) is read first, then the whole descriptor once it says it is that big.
    let size = unsafe { (&raw const (*listener).size).read_unaligned() } as usize;
    if size != size_of::<ts_plugin_api::Plugin>() {
        return INVALID;
    }
    // SAFETY: as above; `size_of::<Plugin>()` bytes are readable. Its callbacks are the plugin's,
    // callable on the engine's thread until it removes the listener (the table's contract).
    let desc = unsafe { listener.read_unaligned() };
    crate::plugins::add_listener(plugin, desc)
}

extern "C" fn remove_listener(handle: c_int) -> c_int {
    if ts_server::plugins::remove_listener(handle) { OK } else { INVALID }
}

extern "C" fn private_size(entity: c_int) -> c_int {
    api::private_size(entity)
}

// ---- version 6: the frame's world ------------------------------------------------------------------

/// Copies `list[..max]` to the caller's `out` (NULL: nothing) and returns the list's length.
///
/// # Safety
/// A non-NULL `out` is writable for `max` entries (the table's contract).
unsafe fn put_list<T: Copy>(list: &[T], out: *mut T, max: c_int) -> c_int {
    if !out.is_null() {
        for (i, x) in list.iter().take(max.max(0) as usize).enumerate() {
            // SAFETY: the caller's guarantee, and `i < max`.
            unsafe { out.add(i).write_unaligned(*x) };
        }
    }
    list.len().min(c_int::MAX as usize) as c_int
}

unsafe extern "C" fn players(out: *mut PlayerSnapshot, max: c_int) -> c_int {
    // SAFETY: the table's contract for `out` and `max`.
    unsafe { put_list(&api::frame::players(), out, max) }
}

unsafe extern "C" fn items(out: *mut ItemInfo, max: c_int) -> c_int {
    // SAFETY: the table's contract for `out` and `max`.
    unsafe { put_list(&api::frame::items(), out, max) }
}

unsafe extern "C" fn frame_info(out: *mut FrameInfo) -> c_int {
    if out.is_null() {
        return INVALID;
    }
    let f = api::frame::frame_info();
    // SAFETY: the caller passes a `FrameInfo` valid for writes (the table's contract).
    unsafe { out.write_unaligned(f) };
    OK
}

unsafe extern "C" fn entity_key(entity: c_int, key: *const c_char, buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(key) = (unsafe { arg(key) }) else { return INVALID };
    match api::keys::entity_key(entity, key) {
        // SAFETY: the table's contract for the buffer. The value's bytes go out as they came
        // (a map's keys aren't UTF-8 necessarily).
        Ok(v) => unsafe { put_bytes(buf, len, v.as_bytes()) },
        Err(code) => code,
    }
}

// ---- version 7: plugin identities ------------------------------------------------------------------

/// Guard: NULL and a name that isn't UTF-8 are `INVALID` (a valid name is ASCII).
unsafe extern "C" fn attach_plugin(name: *const c_char) -> PluginId {
    // SAFETY: the table's contract for string arguments.
    let Some(name) = (unsafe { arg(name) }).and_then(|n| n.to_str().ok()) else { return INVALID };
    ts_server::plugins::attach_plugin(name)
}

extern "C" fn detach_plugin(plugin: PluginId) -> c_int {
    if ts_server::plugins::detach_plugin(plugin) { OK } else { INVALID }
}

// ---- version 8: client extensions, the port client's version --------------------------------------

unsafe extern "C" fn ext_message(player: c_int, channel: *const c_char, data: *const u8, len: c_int, flags: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(channel) = (unsafe { arg(channel) }) else { return INVALID };
    let payload: &[u8] = match (data.is_null(), len) {
        (_, 0) => &[],
        (false, n) if n > 0 => {
            // SAFETY: the table's contract: a non-NULL `data` is readable for `len` bytes.
            unsafe { std::slice::from_raw_parts(data, n as usize) }
        }
        _ => return INVALID,
    };
    api::ext::ext_message(player, channel.to_bytes(), payload, flags)
}

unsafe extern "C" fn client_port_version(player: c_int, buf: *mut c_char, len: c_int) -> c_int {
    match api::ext::client_port_version(player) {
        // SAFETY: the table's contract for the buffer.
        Ok(v) => unsafe { put_str(buf, len, &v) },
        Err(code) => code,
    }
}

/// Guard: an `id` that isn't UTF-8 counts as no offer (`INVALID`): offer ids are ASCII.
unsafe extern "C" fn offer_client_plugin(player: c_int, id: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let id = unsafe { arg(id) }.map(|i| i.to_str().unwrap_or(""));
    api::client_plugins::offer_client_plugin(player, id)
}

/// Guard: an `id` that isn't UTF-8 names no plugin the client listed (its `tspkg` came as text).
unsafe extern "C" fn client_plugin_version(player: c_int, id: *const c_char, buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let id = unsafe { arg(id) }.map(|i| i.to_str().unwrap_or(""));
    match api::client_plugins::client_plugin_version(player, id) {
        // SAFETY: the table's contract for the buffer.
        Ok(v) => unsafe { put_str(buf, len, &v) },
        Err(code) => code,
    }
}
