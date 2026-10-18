//! Client-side engine handle (`cl_enginefunc_t`) and helpers.

use crate::Global;
use crate::sys::*;
use crate::util::to_cstring;
use std::cell::{RefCell, UnsafeCell};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

pub struct ClEngine {
    pub f: cl_enginefunc_t,
}
// SAFETY: the table holds engine function pointers that live for the process; the engine runs
// every client callback on one thread.
unsafe impl Send for ClEngine {}
// SAFETY: as above.
unsafe impl Sync for ClEngine {}

static ENGINE: OnceLock<ClEngine> = OnceLock::new();

/// Called from the client library's `Initialize` export.
///
/// # Safety
/// `funcs` must point to a valid client engine function table.
pub unsafe fn init(funcs: *const cl_enginefunc_t) {
    // SAFETY: `funcs` points at the engine's function table for the duration of this call; it's copied once.
    let _ = ENGINE.set(ClEngine { f: unsafe { *funcs } });
}

pub fn engine() -> &'static ClEngine {
    ENGINE.get().expect("Initialize not called yet")
}

/// Calls a client engine slot, panicking with its name if the engine left it null.
#[macro_export]
macro_rules! cl_eng {
    ($name:ident ( $($arg:expr),* $(,)? )) => {
        // SAFETY: the engine fills its function tables before the first callback and keeps them

        // for the process; slots are called with the argument types bindgen generated from the

        // engine headers, and callers pass valid pointers for those arguments.

        unsafe { ($crate::client::engine().f.$name.expect(stringify!($name)))($($arg),*) }
    };
}

impl ClEngine {
    pub fn print(&self, msg: &str) {
        let s = to_cstring(msg);
        cl_eng!(pfnConsolePrint(s.as_ptr()))
    }

    /// Registers a console command. `f` is a safe `extern "C" fn()` that reads its arguments
    /// through `cmd_argc`/`cmd_argv`. The engine keeps `name`, so it must be `'static`.
    ///
    /// Every command goes through one trampoline that finds `f` by `argv[0]` and asks the command
    /// filter first ([`set_command_filter`]). The first registration of a name wins, as in the
    /// engines, which refuse (with a warning) a name they already have.
    pub fn add_command(&self, name: &'static CStr, f: extern "C" fn()) {
        {
            let mut cmds = COMMANDS.0.borrow_mut();
            if !cmds.iter().any(|(n, _)| n.to_bytes().eq_ignore_ascii_case(name.to_bytes())) {
                cmds.push((name, f));
            }
        }
        cl_eng!(pfnAddCommand(name.as_ptr(), Some(command_trampoline)));
    }

    /// True on FWGS (Xash3D), which has the `host_ver` cvar; false on Steam GoldSrc. Guards for
    /// what only one engine stops on (`cl_guard`) check it.
    pub fn is_xash(&self) -> bool {
        static XASH: OnceLock<bool> = OnceLock::new();
        *XASH.get_or_init(|| !Cvar::find(c"host_ver").is_null())
    }

    /// Logs a guarded call the wrapper refused (port rule: never crash), once per `key` so a
    /// refusal repeated every frame doesn't flood the console.
    pub fn port_guard(&self, key: &str, msg: impl FnOnce() -> String) {
        if crate::cl_guard::once(key) {
            self.print(&format!("{} (port guard)\n", msg()));
        }
    }

    /// Hooks a server user message. The engine calls one shared trampoline with the message
    /// name; it finds `f` by name and passes the payload as a byte slice. Return value: the
    /// engine ignores it, so the handler has none. Guarded: FWGS `Host_Error`s on a new name past
    /// its 197 message slots, so a hook past them is refused there.
    pub fn hook_user_msg(&self, name: &'static CStr, f: fn(&[u8])) {
        let mut hooks = USER_MSG_HOOKS.0.borrow_mut();
        let known = hooks.iter().any(|(n, _)| n.to_bytes().eq_ignore_ascii_case(name.to_bytes()));
        if !known && self.is_xash() && hooks.len() >= crate::cl_guard::XASH_MAX_USER_MESSAGES {
            drop(hooks);
            let n = name.to_string_lossy();
            self.port_guard(&format!("HookUserMsg {n}"), || format!("HookUserMsg \"{n}\": no message slot left; skipped"));
            return;
        }
        match hooks.iter_mut().find(|(n, _)| *n == name) {
            Some(h) => h.1 = f,
            None => hooks.push((name, f)),
        }
        drop(hooks);
        cl_eng!(pfnHookUserMsg(name.as_ptr(), Some(user_msg_trampoline)));
    }

    pub fn cvar_float(&self, name: &CStr) -> f32 {
        cl_eng!(pfnGetCvarFloat(name.as_ptr()))
    }

    pub fn view_angles(&self) -> [f32; 3] {
        let mut a = [0.0; 3];
        cl_eng!(GetViewAngles(a.as_mut_ptr()));
        a
    }

    /// `GetClientMaxspeed`: the local player's `maxspeed` from the last client data.
    pub fn client_maxspeed(&self) -> f32 {
        cl_eng!(GetClientMaxspeed())
    }

    pub fn cmd_argv(&self, i: i32) -> &'static CStr {
        let p = cl_eng!(Cmd_Argv(i));
        // SAFETY: the engine returns null or a NUL-terminated string it owns; it's used or copied before the next engine call.
        unsafe { crate::util::cstr(p) }
    }

    pub fn angle_vectors(&self, angles: [f32; 3]) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let (mut f, mut r, mut u) = ([0.0; 3], [0.0; 3], [0.0; 3]);
        cl_eng!(pfnAngleVectors(angles.as_ptr(), f.as_mut_ptr(), r.as_mut_ptr(), u.as_mut_ptr()));
        (f, r, u)
    }
}

/// The client command filter ([`set_command_filter`]): asked with `argv[0]` before a command
/// registered through [`ClEngine::add_command`] runs; false swallows it.
pub type CommandFilter = fn(name: &CStr) -> bool;

type Command = (&'static CStr, extern "C" fn());
static COMMANDS: Global<RefCell<Vec<Command>>> = Global(RefCell::new(Vec::new()));
static COMMAND_FILTER: Global<std::cell::Cell<Option<CommandFilter>>> = Global(std::cell::Cell::new(None));

/// Installs the filter every command of [`ClEngine::add_command`] passes first. Port addition:
/// the client extensions' `command_pre` hook.
pub fn set_command_filter(f: Option<CommandFilter>) {
    COMMAND_FILTER.0.set(f);
}

/// The one function the engine has for every command the game registered: the command is found
/// by the name it was run as (the engines match names case-insensitively).
extern "C" fn command_trampoline() {
    let name = engine().cmd_argv(0);
    // Copied out, so the command may register commands itself.
    let f = COMMANDS.0.borrow().iter().find(|(n, _)| n.to_bytes().eq_ignore_ascii_case(name.to_bytes())).map(|c| c.1);
    let Some(f) = f else { return };
    if let Some(filter) = COMMAND_FILTER.0.get()
        && !filter(name)
    {
        return;
    }
    f();
}

type UserMsgHook = (&'static CStr, fn(&[u8]));
static USER_MSG_HOOKS: Global<RefCell<Vec<UserMsgHook>>> = Global(RefCell::new(Vec::new()));

/// The one `pfnUserMsgHook` every hooked message goes through (GoldSrc and FWGS pass the
/// message name to the hook).
unsafe extern "C" fn user_msg_trampoline(name: *const c_char, size: c_int, buf: *mut c_void) -> c_int {
    // SAFETY: the engine passes the message name and its payload buffer of `size` bytes.
    let (name, bytes) = unsafe {
        let name = crate::util::cstr(name);
        let bytes: &[u8] = if buf.is_null() || size <= 0 { &[] } else { std::slice::from_raw_parts(buf.cast(), size as usize) };
        (name, bytes)
    };
    // Copy the handler out so it can hook or unhook messages itself.
    let f = USER_MSG_HOOKS.0.borrow().iter().find(|(n, _)| *n == name).map(|h| h.1);
    if let Some(f) = f {
        f(bytes);
    }
    1
}

/// A cvar registered with or found in the engine: the one cvar handle type, and the one
/// `pfnRegisterVariable` / `pfnGetCvarPointer` wrapper. The engine owns the `cvar_t` for the rest
/// of the process, so the handle is `Copy`; a null handle reads as 0.
///
/// Registration rule: as TS (and the SDK's `CVAR_CREATE`), each cvar is registered once, with
/// [`Cvar::register`], where TS registers it (decomp architecture-hud.md §2.1); every other
/// reader shares that handle or looks it up with [`Cvar::find`]. Registering a name twice is not
/// harmless: FWGS's `Cvar_Get` returns the existing cvar but resets it to the new default and ORs
/// in the new flags, and GoldSrc's `Cvar_RegisterVariable` (as in ReHLDS) refuses it ("already
/// defined").
#[derive(Clone, Copy)]
pub struct Cvar(*mut cvar_s);
// SAFETY: the cvar_t is engine-owned for the process and only touched on the client thread.
unsafe impl Send for Cvar {}
// SAFETY: as above.
unsafe impl Sync for Cvar {}

impl Cvar {
    pub const NULL: Cvar = Cvar(std::ptr::null_mut());

    /// `pfnRegisterVariable`, as TS calls it. Call once per name (see the type docs). The engine
    /// keeps `name` and `value`, so they must be `'static`.
    pub fn register(name: &'static CStr, value: &'static CStr, flags: i32) -> Cvar {
        Cvar(cl_eng!(pfnRegisterVariable(name.as_ptr(), value.as_ptr(), flags)))
    }

    /// `pfnGetCvarPointer`: a cvar registered elsewhere (another module or the engine), or a
    /// null handle.
    pub fn find(name: &CStr) -> Cvar {
        Cvar(cl_eng!(pfnGetCvarPointer(name.as_ptr())))
    }

    /// [`Cvar::find`] without the engine's complaint when there's no such cvar: walks the cvar
    /// list from `pfnGetFirstCvarPtr` (both engines; FWGS's `convar_t` starts with `cvar_t`'s
    /// fields, `next` included), matching names case-insensitively as the engines do. FWGS's
    /// `CL_CvarGetPointer` warns "client tried to get non-existent cvar" for every miss, which is
    /// the normal case when checking a new name for a clash (issue #315). Falls back to
    /// [`Cvar::find`] on an engine without the list.
    pub fn lookup(name: &CStr) -> Cvar {
        let Some(first) = engine().f.pfnGetFirstCvarPtr else { return Cvar::find(name) };
        // SAFETY: the engine's function, no arguments; it returns the head of its cvar list or NULL.
        let mut p = unsafe { first() };
        // A bound in case the list were ever circular: no engine has this many cvars.
        for _ in 0..65536 {
            // SAFETY: list entries are engine-owned cvars that live for the process; `name` is
            // NUL-terminated or NULL, and `next` NULL or the next entry. The client thread runs
            // no engine code while this walks.
            let Some(c) = (unsafe { p.as_ref() }) else { break };
            // SAFETY: as above.
            if !c.name.is_null() && unsafe { CStr::from_ptr(c.name) }.to_bytes().eq_ignore_ascii_case(name.to_bytes()) {
                return Cvar(p);
            }
            p = c.next;
        }
        Cvar::NULL
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }

    pub fn value(self) -> f32 {
        // SAFETY: non-null cvar pointers come from the engine and live for the process.
        unsafe { self.0.as_ref() }.map_or(0.0, |c| c.value)
    }

    /// `cvar->string`, `None` for a null handle or string. Copy it before the cvar is next set.
    pub fn string(self) -> Option<&'static CStr> {
        // SAFETY: non-null cvar pointers come from the engine and live for the process; the
        // engine keeps `string` null or NUL-terminated, and frees it only when the cvar is set,
        // which can't happen on the client thread while the caller holds the reference.
        let s = unsafe { self.0.as_ref() }?.string;
        // SAFETY: as above.
        (!s.is_null()).then(|| unsafe { CStr::from_ptr(s) })
    }

    /// Writes `cvar->value` directly, as the SDK's and TS's camera code does (`snapto`,
    /// `CAM_Think`'s `cam_ideal*` updates): the string is left alone and the engine isn't told.
    /// Use `Cvar_SetValue` (`ClEngine::cvar_set`) for a normal change. No-op on a null handle.
    pub fn set_value(self, v: f32) {
        // SAFETY: non-null cvar pointers come from the engine and live for the process; the client
        // runs on one thread and holds no other reference to the cvar across this write.
        if let Some(c) = unsafe { self.0.as_mut() } {
            c.value = v;
        }
    }
}

/// Storage the engine also reads and writes through a pointer we hand it, such as the
/// `kbutton_t`s returned from `KB_Find`. Access is single-threaded (engine callbacks), and a
/// borrow from [`EngineCell::get`] must not be held across a call back into the engine.
pub struct EngineCell<T>(UnsafeCell<T>);
// SAFETY: only used from the engine's single client thread.
unsafe impl<T> Sync for EngineCell<T> {}

impl<T> EngineCell<T> {
    pub const fn new(v: T) -> Self {
        EngineCell(UnsafeCell::new(v))
    }

    #[allow(clippy::mut_from_ref)]
    pub fn get(&self) -> &mut T {
        // SAFETY: single-threaded engine callbacks; callers keep borrows short (see type docs).
        unsafe { &mut *self.0.get() }
    }

    /// The pointer to give the engine.
    pub fn as_ptr(&self) -> *mut T {
        self.0.get()
    }
}
