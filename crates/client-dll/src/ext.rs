//! Client extensions (port addition, issue #293): the FFI half of `ts_client::ext`. Which files
//! load, the approval gate and when each callback runs are decided there; this module opens a
//! library, calls its `TSRS_ClientExtLoad`, turns its C callbacks into
//! `ts_client::ext::ExtHooks`, and builds the `ClientExtApi` table, whose functions convert their
//! arguments and call `ts_client::ext` (docs/implementation/client-ext-api.md).

use goldsrc::client::{Cvar, engine};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::rc::Rc;
use std::sync::OnceLock;
use ts_client::ext::gate::Candidate;
use ts_client::ext::{self as ext, ExtHooks, view};
use ts_client_ext_api::{
    AimInfo, ClientExt, ClientExtApi, CommandFn, ExtId, GameEvent, INVALID, LOAD_SYMBOL, LoadFn, LoadInfo, LocalState, MINOR, OK,
    PlayerScore, TraceInfo, UserCmd, VERSION, ViewInfo, VisiblePlayer, renderer,
};

/// `Initialize`: the approved extensions (`ts_client::ext::load_all`).
pub fn load_all() {
    ext::load_all(&open);
}

/// Opens one approved library and runs its load function. The library stays mapped for the
/// process once its load function has run (it may have handed out pointers into itself even
/// when it refused).
fn open(c: &Candidate, id: ExtId) -> Result<(Rc<dyn ExtHooks>, String), String> {
    // SAFETY: opening runs the library's initialisers. The loader checked its platform from its
    // header, and the approval gate approved it (the player's consent to run this code).
    let lib = unsafe { libloading::Library::new(&c.library) }.map_err(|e| format!("can't be loaded ({e})"))?;
    // SAFETY: `TSRS_ClientExtLoad` has the type `LoadFn` by the API's contract; the pointer is
    // copied out and only used while the library stays mapped (for the process, below).
    let entry: LoadFn = match unsafe { lib.get::<LoadFn>(LOAD_SYMBOL.to_bytes_with_nul()) } {
        Ok(f) => *f,
        Err(_) => return Err(format!("has no {} export", LOAD_SYMBOL.to_string_lossy())),
    };
    let cname = CString::new(c.name.as_str()).unwrap_or_default();
    let cpath = CString::new(c.library.to_string_lossy().into_owned()).unwrap_or_default();
    let info = LoadInfo { id, name: cname.as_ptr(), path: cpath.as_ptr() };
    let mut desc = ext::blank_descriptor();
    let api = table();
    // SAFETY: `entry` is the extension's load function; `api` is the static table, `info` and
    // `desc` live for the call, and `desc` is `size_of::<ClientExt>()` bytes as passed.
    let rc = unsafe { entry(api, api.size, &info, &mut desc, size_of::<ClientExt>() as u32) };
    std::mem::forget(lib);
    ext::check_loaded(rc, &desc)?;
    // SAFETY: the descriptor's strings are NULL or static NUL-terminated strings (its contract).
    let (title, version) = unsafe { (text(desc.name), text(desc.version)) };
    let title = if title.is_empty() { c.name.clone() } else { title };
    let d = if version.is_empty() { title } else { format!("{title} {version}") };
    Ok((Rc::new(Loaded { desc }), d))
}

/// A descriptor's string, for the console.
///
/// # Safety
/// `p` is NULL or a NUL-terminated string.
unsafe fn text(p: *const c_char) -> String {
    // SAFETY: the caller's guarantee.
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// A loaded extension's callbacks. The safe ones are called directly; each `unsafe` one gets its
/// arguments as the descriptor's contract says.
struct Loaded {
    desc: ClientExt,
}

impl ExtHooks for Loaded {
    fn init(&self) {
        if let Some(f) = self.desc.init {
            f();
        }
    }
    fn vid_init(&self) {
        if let Some(f) = self.desc.vid_init {
            f();
        }
    }
    fn frame(&self, time: f64) {
        if let Some(f) = self.desc.frame {
            f(time);
        }
    }
    fn redraw(&self, time: f32, intermission: bool) {
        if let Some(f) = self.desc.redraw {
            f(time, intermission as c_int);
        }
    }
    fn key_event(&self, down: bool, key: i32, binding: &CStr) -> bool {
        let Some(f) = self.desc.key_event else { return false };
        // SAFETY: `binding` is a NUL-terminated string that lives for the call.
        unsafe { f(down as c_int, key, binding.as_ptr()) != 0 }
    }
    fn mouse_active(&self, active: bool) {
        if let Some(f) = self.desc.mouse_active {
            f(active as c_int);
        }
    }
    fn wants_cursor(&self) -> bool {
        self.desc.wants_cursor.is_some_and(|f| f() != 0)
    }
    fn usercmd(&self, cmd: &UserCmd) {
        if let Some(f) = self.desc.usercmd {
            // SAFETY: `cmd` is a `UserCmd` that lives for the call.
            unsafe { f(cmd) };
        }
    }
    fn game_event(&self, event: &GameEvent) {
        if let Some(f) = self.desc.game_event {
            // SAFETY: `event` is a `GameEvent` that lives for the call.
            unsafe { f(event) };
        }
    }
    fn suspend(&self, reason: &CStr) {
        if let Some(f) = self.desc.suspend {
            // SAFETY: `reason` is a NUL-terminated string that lives for the call.
            unsafe { f(reason.as_ptr()) };
        }
    }
    fn resume(&self) {
        if let Some(f) = self.desc.resume {
            f();
        }
    }
    fn server_changed(&self, address: &CStr) {
        if let Some(f) = self.desc.server_changed {
            // SAFETY: `address` is a NUL-terminated string that lives for the call.
            unsafe { f(address.as_ptr()) };
        }
    }
    fn shutdown(&self) {
        if let Some(f) = self.desc.shutdown {
            f();
        }
    }
}

// ---- the table ----------------------------------------------------------------------------------

/// The table: function pointers only.
struct Table(ClientExtApi);
// SAFETY: the table holds function pointers into this library, which lives for the process;
// extensions only read it, on the engine's thread.
unsafe impl Sync for Table {}
// SAFETY: as above.
unsafe impl Send for Table {}

/// The one table.
pub fn table() -> &'static ClientExtApi {
    static TABLE: OnceLock<Table> = OnceLock::new();
    &TABLE.get_or_init(|| Table(build())).0
}

fn build() -> ClientExtApi {
    ClientExtApi {
        size: size_of::<ClientExtApi>() as u32,
        version: VERSION,
        minor: MINOR,
        print,
        add_command,
        cmd_argc,
        cmd_argv,
        register_cvar,
        cvar_value,
        cvar_string,
        set_cvar_value,
        client_cmd,
        server_cmd,
        screen_size,
        mouse_position,
        gl_proc,
        renderer_kind,
        game_dir,
        port_version,
        server_address,
        data_dir,
        verified_spawn,
        local_state,
        player,
        scores,
        aim,
        trace_line,
        visible_players,
        view: view_info,
        world_to_screen,
        weapon_name,
        max_clients,
        reserved: [None; ts_client_ext_api::API_RESERVED],
    }
}

/// A string argument: `None` for NULL.
///
/// # Safety
/// `p` is NULL or a NUL-terminated string valid for the call (the table's contract).
unsafe fn arg<'a>(p: *const c_char) -> Option<&'a CStr> {
    // SAFETY: the caller's guarantee.
    (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) })
}

/// Writes `s` into the caller's buffer, NUL-terminated and cut to fit, and returns its full
/// length. Guard: a NULL buffer or a length below 1 only gives the length.
///
/// # Safety
/// A non-NULL `buf` is writable for `len` bytes (the table's contract).
unsafe fn put_bytes(buf: *mut c_char, len: c_int, s: &[u8]) -> c_int {
    if !buf.is_null() && len > 0 {
        let n = s.len().min(len as usize - 1);
        // SAFETY: `buf` is writable for `len` bytes and `n + 1 <= len`; `s` doesn't overlap it.
        unsafe {
            std::ptr::copy_nonoverlapping(s.as_ptr(), buf.cast::<u8>(), n);
            *buf.add(n) = 0;
        }
    }
    s.len() as c_int
}

/// Writes `v` to a caller's out-pointer: `OK`, or `INVALID` for NULL.
///
/// # Safety
/// A non-NULL `out` is writable for one `T` (the table's contract).
unsafe fn put<T>(out: *mut T, v: T) -> c_int {
    if out.is_null() {
        return INVALID;
    }
    // SAFETY: the caller's guarantee; `write` doesn't read the (possibly uninitialised) old value.
    unsafe { out.write(v) };
    OK
}

/// [`put`] of a query's result, or its error code.
///
/// # Safety
/// As [`put`].
unsafe fn put_result<T>(out: *mut T, r: Result<T, i32>) -> c_int {
    match r {
        // SAFETY: the caller's guarantee.
        Ok(v) => unsafe { put(out, v) },
        Err(e) => e,
    }
}

/// Copies up to `max` of `items` to `out` and returns how many there are in all.
///
/// # Safety
/// A non-NULL `out` is writable for `max` `T`s (the table's contract).
unsafe fn put_list<T: Copy>(out: *mut T, max: c_int, items: &[T]) -> c_int {
    if !out.is_null() && max > 0 {
        let n = items.len().min(max as usize);
        // SAFETY: the caller's guarantee; `n <= max` and `items` doesn't overlap `out`.
        unsafe { std::ptr::copy_nonoverlapping(items.as_ptr(), out, n) };
    }
    items.len() as c_int
}

/// Three floats from a caller's pointer.
///
/// # Safety
/// A non-NULL `p` points at three readable floats (the table's contract).
unsafe fn vec3(p: *const f32) -> Option<[f32; 3]> {
    // SAFETY: the caller's guarantee.
    (!p.is_null()).then(|| unsafe { p.cast::<[f32; 3]>().read_unaligned() })
}

unsafe extern "C" fn print(id: ExtId, msg: *const c_char) {
    // SAFETY: the table's contract for string arguments.
    if let Some(m) = unsafe { arg(msg) } {
        ext::host_print(id, m.to_bytes());
    }
}

unsafe extern "C" fn add_command(id: ExtId, name: *const c_char, f: CommandFn) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(name) = (unsafe { arg(name) }).and_then(|n| n.to_str().ok()) else { return INVALID };
    ext::host_add_command(id, name, f)
}

extern "C" fn cmd_argc() -> c_int {
    engine().cmd_argc()
}

extern "C" fn cmd_argv(i: c_int) -> *const c_char {
    // The engine's string, valid until the next command runs (the table's contract: copy it).
    engine().cmd_argv(i).as_ptr()
}

unsafe extern "C" fn register_cvar(id: ExtId, name: *const c_char, value: *const c_char, flags: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let (Some(name), value) = (unsafe { arg(name) }, unsafe { arg(value) }) else { return INVALID };
    let Ok(name) = name.to_str() else { return INVALID };
    let value = value.map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
    ext::host_register_cvar(id, name, &value, flags)
}

unsafe extern "C" fn cvar_value(name: *const c_char) -> f32 {
    // SAFETY: the table's contract for string arguments.
    unsafe { arg(name) }.map_or(0.0, |n| Cvar::find(n).value())
}

unsafe extern "C" fn cvar_string(name: *const c_char, buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let s = unsafe { arg(name) }.and_then(|n| Cvar::find(n).string()).map(|s| s.to_bytes().to_vec()).unwrap_or_default();
    // SAFETY: the table's contract for buffers.
    unsafe { put_bytes(buf, len, &s) }
}

unsafe extern "C" fn set_cvar_value(name: *const c_char, value: f32) -> c_int {
    // SAFETY: the table's contract for string arguments.
    match unsafe { arg(name) } {
        Some(n) => ext::host_set_cvar(n, value),
        None => INVALID,
    }
}

unsafe extern "C" fn client_cmd(id: ExtId, cmd: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    match unsafe { arg(cmd) } {
        Some(c) => ext::host_cmd(id, &c.to_string_lossy(), false),
        None => INVALID,
    }
}

unsafe extern "C" fn server_cmd(id: ExtId, cmd: *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    match unsafe { arg(cmd) } {
        Some(c) => ext::host_cmd(id, &c.to_string_lossy(), true),
        None => INVALID,
    }
}

unsafe extern "C" fn screen_size(width: *mut c_int, height: *mut c_int) {
    let s = ts_client::hud::screen();
    // SAFETY: the table's contract for out-pointers (NULL ones are skipped).
    unsafe {
        put(width, s.width);
        put(height, s.height);
    }
}

unsafe extern "C" fn mouse_position(x: *mut c_int, y: *mut c_int) {
    let (mx, my) = ext::host_mouse_position();
    // SAFETY: the table's contract for out-pointers (NULL ones are skipped).
    unsafe {
        put(x, mx);
        put(y, my);
    }
}

unsafe extern "C" fn gl_proc(name: *const c_char) -> *mut c_void {
    // SAFETY: the table's contract for string arguments.
    unsafe { arg(name) }.map_or(std::ptr::null_mut(), goldsrc::ext::gl::proc_address)
}

extern "C" fn renderer_kind() -> c_int {
    match goldsrc::ext::gl::renderer() {
        goldsrc::ext::gl::Renderer::Software => renderer::SOFTWARE,
        goldsrc::ext::gl::Renderer::Gl => renderer::GL,
        goldsrc::ext::gl::Renderer::Gles => renderer::GLES,
    }
}

unsafe extern "C" fn game_dir(buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for buffers.
    unsafe { put_bytes(buf, len, engine().game_directory().as_bytes()) }
}

unsafe extern "C" fn port_version(buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for buffers.
    unsafe { put_bytes(buf, len, shared::protocol::port::VERSION.as_bytes()) }
}

unsafe extern "C" fn server_address(buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for buffers.
    unsafe { put_bytes(buf, len, ext::host_server_address().as_bytes()) }
}

unsafe extern "C" fn data_dir(id: ExtId, buf: *mut c_char, len: c_int) -> c_int {
    match ext::host_data_dir(id) {
        // SAFETY: the table's contract for buffers.
        Ok(d) => unsafe { put_bytes(buf, len, d.as_bytes()) },
        Err(e) => e,
    }
}

unsafe extern "C" fn verified_spawn(id: ExtId, path: *const c_char, argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: the table's contract for string arguments.
    let Some(path) = (unsafe { arg(path) }) else { return INVALID };
    let mut args = Vec::new();
    if argc > 0 && !argv.is_null() {
        for i in 0..argc as usize {
            // SAFETY: `argv` holds `argc` string pointers (the table's contract), each NULL or
            // NUL-terminated.
            let a = unsafe { arg(*argv.add(i)) };
            args.push(a.map(|a| a.to_string_lossy().into_owned()).unwrap_or_default());
        }
    }
    ext::host_verified_spawn(id, &path.to_string_lossy(), &args)
}

unsafe extern "C" fn local_state(out: *mut LocalState) -> c_int {
    // SAFETY: the table's contract for out-pointers.
    unsafe { put_result(out, view::local_state()) }
}

unsafe extern "C" fn player(slot: c_int, out: *mut PlayerScore) -> c_int {
    // SAFETY: the table's contract for out-pointers.
    unsafe { put_result(out, view::player_score(slot)) }
}

unsafe extern "C" fn scores(out: *mut PlayerScore, max: c_int) -> c_int {
    // SAFETY: the table's contract for lists.
    unsafe { put_list(out, max, &view::scores()) }
}

unsafe extern "C" fn aim(out: *mut AimInfo) -> c_int {
    // SAFETY: the table's contract for out-pointers.
    unsafe { put_result(out, view::aim()) }
}

unsafe extern "C" fn trace_line(start: *const f32, end: *const f32, out: *mut TraceInfo) -> c_int {
    // SAFETY: the table's contract for vectors.
    let (Some(s), Some(e)) = (unsafe { vec3(start) }, unsafe { vec3(end) }) else { return INVALID };
    // SAFETY: the table's contract for out-pointers.
    unsafe { put_result(out, view::trace_line(s, e)) }
}

unsafe extern "C" fn visible_players(out: *mut VisiblePlayer, max: c_int) -> c_int {
    // SAFETY: the table's contract for lists.
    unsafe { put_list(out, max, &ext::host_visible_players()) }
}

unsafe extern "C" fn view_info(out: *mut ViewInfo) -> c_int {
    // SAFETY: the table's contract for out-pointers.
    unsafe { put(out, view::view()) }
}

unsafe extern "C" fn world_to_screen(point: *const f32, out: *mut f32) -> c_int {
    // SAFETY: the table's contract for vectors.
    let Some(p) = (unsafe { vec3(point) }) else { return INVALID };
    match view::world_to_screen(p) {
        // SAFETY: the table's contract: `out` is writable for two floats.
        Ok(xy) => unsafe { put(out.cast::<[f32; 2]>(), xy) },
        Err(e) => e,
    }
}

unsafe extern "C" fn weapon_name(id: c_int, buf: *mut c_char, len: c_int) -> c_int {
    // SAFETY: the table's contract for buffers.
    unsafe { put_bytes(buf, len, view::weapon_name(id).as_bytes()) }
}

extern "C" fn max_clients() -> c_int {
    engine().max_clients()
}
