//! Raw OpenGL calls, for the few places TS's client calls `opengl32.dll` itself rather than going
//! through the engine: `V_SetFog` (`0x10056c20`: `glFogi`, `glFogfv`, `glFogf`, `glHint`,
//! `glEnable`) and the `glDisable(GL_FOG)` that ends `TS_DrawEffects` (`0x10056e5e`). They act on
//! the GL state behind the engine's back, as TS's do.
//!
//! The functions are looked up at run time in the GL library the engine's renderer already
//! loaded, never loading one:
//!
//! * Windows (Steam `hw.dll`, FWGS `ref_gl` under Windows or Wine): `opengl32.dll`, which exports
//!   these GL 1.1 entry points directly, as TS imports them;
//! * Linux (Steam `hw.so`, FWGS `ref_gl`): `SDL_GL_GetProcAddress` from the SDL2 the engine
//!   loaded (the GL library SDL loaded for the context), else `libGL.so.1`/`libOpenGL.so.0` if
//!   already loaded.
//!
//! Every call does nothing unless the engine is drawing with desktop OpenGL ([`active`]): FWGS's
//! loaded renderer (`r_refdll_loaded`) is `gl`, or, on engines without that cvar (Steam
//! GoldSrc), `IEngineStudio.IsHardware()` is 1 (`hw`, not the software `sw`). FWGS's software
//! renderer and its GLES renderers (`gles1`, `gles2`, `gl4es`, `gles3compat`, which translate GL
//! through a wrapper the raw calls would bypass) get nothing. A function the library doesn't
//! export is skipped on its own.
//!
//! Call only from the engine's render callbacks (`V_CalcRefdef`, `HUD_DrawTransparentTriangles`),
//! where the renderer's context is current on the calling thread.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

pub const GL_FOG: u32 = 0x0b60;
pub const GL_FOG_DENSITY: u32 = 0x0b62;
pub const GL_FOG_START: u32 = 0x0b63;
pub const GL_FOG_END: u32 = 0x0b64;
pub const GL_FOG_MODE: u32 = 0x0b65;
pub const GL_FOG_COLOR: u32 = 0x0b66;
pub const GL_FOG_HINT: u32 = 0x0c54;
pub const GL_NICEST: u32 = 0x1102;
pub const GL_LINEAR: i32 = 0x2601;

// `APIENTRY`: stdcall on 32-bit Windows, C elsewhere.
type Cap = unsafe extern "system" fn(u32);
type Fogi = unsafe extern "system" fn(u32, i32);
type Fogf = unsafe extern "system" fn(u32, f32);
type Fogfv = unsafe extern "system" fn(u32, *const f32);
type Hint = unsafe extern "system" fn(u32, u32);

struct Gl {
    enable: Option<Cap>,
    disable: Option<Cap>,
    fogi: Option<Fogi>,
    fogf: Option<Fogf>,
    fogfv: Option<Fogfv>,
    hint: Option<Hint>,
}

/// The engine draws with desktop OpenGL (see the module docs).
pub fn active() -> bool {
    let refdll = crate::client::Cvar::find(c"r_refdll_loaded");
    if !refdll.is_null() {
        return refdll.string().is_some_and(|s| s.to_bytes() == b"gl");
    }
    crate::ext::studio::hardware_kind() == 1
}

/// The GL functions, looked up on the first call made while [`active`] (the renderer can't change
/// without restarting the engine). `None` when GL isn't active.
fn gl() -> Option<&'static Gl> {
    static GL: OnceLock<Gl> = OnceLock::new();
    if let Some(gl) = GL.get() {
        return Some(gl);
    }
    if !active() {
        return None;
    }
    Some(GL.get_or_init(|| {
        let f = |name: &CStr| {
            let p = lookup(name);
            (!p.is_null()).then_some(p)
        };
        Gl {
            // SAFETY: a non-null GL export with this name is the GL 1.1 function with this
            // `APIENTRY` signature (gl.h). The same holds for the five below.
            enable: f(c"glEnable").map(|p| unsafe { std::mem::transmute::<*mut c_void, Cap>(p) }),
            // SAFETY: as above.
            disable: f(c"glDisable").map(|p| unsafe { std::mem::transmute::<*mut c_void, Cap>(p) }),
            // SAFETY: as above.
            fogi: f(c"glFogi").map(|p| unsafe { std::mem::transmute::<*mut c_void, Fogi>(p) }),
            // SAFETY: as above.
            fogf: f(c"glFogf").map(|p| unsafe { std::mem::transmute::<*mut c_void, Fogf>(p) }),
            // SAFETY: as above.
            fogfv: f(c"glFogfv").map(|p| unsafe { std::mem::transmute::<*mut c_void, Fogfv>(p) }),
            // SAFETY: as above.
            hint: f(c"glHint").map(|p| unsafe { std::mem::transmute::<*mut c_void, Hint>(p) }),
        }
    }))
}

#[cfg(windows)]
fn lookup(name: &CStr) -> *mut c_void {
    use crate::platform::win::{GetModuleHandleA, GetProcAddress};
    // SAFETY: GetModuleHandleA takes a NUL-terminated name, only looks up an already-loaded
    // module (it never loads one) and returns null when opengl32.dll isn't loaded.
    let h = unsafe { GetModuleHandleA(c"opengl32.dll".as_ptr()) };
    if h.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `h` is a module handle from GetModuleHandleA and `name` is NUL-terminated;
    // GetProcAddress returns null for a missing export.
    unsafe { GetProcAddress(h, name.as_ptr()) }
}

#[cfg(not(windows))]
fn lookup(name: &CStr) -> *mut c_void {
    use std::ffi::{c_char, c_int};
    unsafe extern "C" {
        fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    }
    const RTLD_NOW: c_int = 2;
    const RTLD_NOLOAD: c_int = 4;
    type GetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;

    let gpa = crate::platform::sdl::lookup(c"SDL_GL_GetProcAddress");
    if !gpa.is_null() {
        // SAFETY: a non-null `SDL_GL_GetProcAddress` is SDL2's function with this C signature.
        let gpa = unsafe { std::mem::transmute::<*mut c_void, GetProcAddress>(gpa) };
        // SAFETY: `name` is NUL-terminated. [`gl`] only gets here while the engine draws with GL,
        // so SDL already has the GL library loaded and doesn't load one now.
        let p = unsafe { gpa(name.as_ptr()) };
        if !p.is_null() {
            return p;
        }
    }
    for lib in [c"libGL.so.1", c"libOpenGL.so.0"] {
        // SAFETY: dlopen/dlsym take NUL-terminated names; RTLD_NOLOAD never loads a new library.
        let p = unsafe {
            let h = dlopen(lib.as_ptr(), RTLD_NOW | RTLD_NOLOAD);
            if h.is_null() { std::ptr::null_mut() } else { dlsym(h, name.as_ptr()) }
        };
        if !p.is_null() {
            return p;
        }
    }
    std::ptr::null_mut()
}

/// `glEnable(cap)`.
pub fn enable(cap: u32) {
    if let Some(f) = gl().and_then(|g| g.enable) {
        // SAFETY: GL function looked up above, called with its signature on the render thread
        // (module docs), where the renderer's context is current.
        unsafe { f(cap) }
    }
}

/// `glDisable(cap)`.
pub fn disable(cap: u32) {
    if let Some(f) = gl().and_then(|g| g.disable) {
        // SAFETY: as in [`enable`].
        unsafe { f(cap) }
    }
}

/// `glFogi(pname, param)`.
pub fn fogi(pname: u32, param: i32) {
    if let Some(f) = gl().and_then(|g| g.fogi) {
        // SAFETY: as in [`enable`].
        unsafe { f(pname, param) }
    }
}

/// `glFogf(pname, param)`.
pub fn fogf(pname: u32, param: f32) {
    if let Some(f) = gl().and_then(|g| g.fogf) {
        // SAFETY: as in [`enable`].
        unsafe { f(pname, param) }
    }
}

/// `glFogfv(pname, params)`: four floats, enough for `GL_FOG_COLOR` (the largest fog parameter).
pub fn fogfv(pname: u32, params: &[f32; 4]) {
    if let Some(f) = gl().and_then(|g| g.fogfv) {
        // SAFETY: as in [`enable`]; GL reads at most four floats for a fog parameter.
        unsafe { f(pname, params.as_ptr()) }
    }
}

/// `glHint(target, mode)`.
pub fn hint(target: u32, mode: u32) {
    if let Some(f) = gl().and_then(|g| g.hint) {
        // SAFETY: as in [`enable`].
        unsafe { f(target, mode) }
    }
}

/// The renderer the engine draws with: what a client extension is told (port addition, issue
/// #293), and whether [`proc_address`] has anything to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renderer {
    Software,
    /// Desktop OpenGL ([`active`]).
    Gl,
    /// One of FWGS's GLES renderers (`gles1`, `gles2`, `gl4es`, `gles3compat`).
    Gles,
}

/// The renderer: FWGS's `r_refdll_loaded`, or on engines without it (Steam GoldSrc) hardware
/// (`hw`) or software.
pub fn renderer() -> Renderer {
    let refdll = crate::client::Cvar::find(c"r_refdll_loaded");
    if !refdll.is_null() {
        return match refdll.string().map(|s| s.to_bytes()) {
            Some(b"gl") => Renderer::Gl,
            Some(b"gles1" | b"gles2" | b"gl4es" | b"gles3compat") => Renderer::Gles,
            _ => Renderer::Software,
        };
    }
    if crate::ext::studio::hardware_kind() == 1 { Renderer::Gl } else { Renderer::Software }
}

/// A GL or GLES function of the renderer's context (looked up as the module docs say, never
/// loading a library), or null for none and on the software renderer. For client extensions,
/// which draw with the renderer's own context.
pub fn proc_address(name: &CStr) -> *mut c_void {
    if renderer() == Renderer::Software { std::ptr::null_mut() } else { lookup(name) }
}

const GL_ALL_ATTRIB_BITS: u32 = 0x000f_ffff;
const GL_CLIENT_ALL_ATTRIB_BITS: u32 = 0xffff_ffff;
const GL_MATRIX_MODE: u32 = 0x0ba0;
const GL_MODELVIEW: u32 = 0x1700;
const GL_PROJECTION: u32 = 0x1701;
const GL_TEXTURE: u32 = 0x1702;

type Mask = unsafe extern "system" fn(u32);
type Void = unsafe extern "system" fn();
type GetIntegerv = unsafe extern "system" fn(u32, *mut i32);

/// The GL 1.1 calls [`with_saved_state`] needs.
struct SaveFns {
    push_attrib: Mask,
    pop_attrib: Void,
    push_client_attrib: Mask,
    pop_client_attrib: Void,
    matrix_mode: Mask,
    push_matrix: Void,
    pop_matrix: Void,
    get_integerv: GetIntegerv,
}

fn save_fns() -> Option<&'static SaveFns> {
    static FNS: OnceLock<Option<SaveFns>> = OnceLock::new();
    if !active() {
        return None;
    }
    FNS.get_or_init(|| {
        let f = |name: &CStr| {
            let p = lookup(name);
            (!p.is_null()).then_some(p)
        };
        // SAFETY: a non-null GL export with each of these names is the GL 1.1 function with the
        // `APIENTRY` signature given here (gl.h).
        unsafe {
            Some(SaveFns {
                push_attrib: std::mem::transmute::<*mut c_void, Mask>(f(c"glPushAttrib")?),
                pop_attrib: std::mem::transmute::<*mut c_void, Void>(f(c"glPopAttrib")?),
                push_client_attrib: std::mem::transmute::<*mut c_void, Mask>(f(c"glPushClientAttrib")?),
                pop_client_attrib: std::mem::transmute::<*mut c_void, Void>(f(c"glPopClientAttrib")?),
                matrix_mode: std::mem::transmute::<*mut c_void, Mask>(f(c"glMatrixMode")?),
                push_matrix: std::mem::transmute::<*mut c_void, Void>(f(c"glPushMatrix")?),
                pop_matrix: std::mem::transmute::<*mut c_void, Void>(f(c"glPopMatrix")?),
                get_integerv: std::mem::transmute::<*mut c_void, GetIntegerv>(f(c"glGetIntegerv")?),
            })
        }
    })
    .as_ref()
}

/// Runs `f` with the GL state saved around it. Port addition for client extensions' `redraw`
/// (issues #293, #306), so an extension can't leave the HUD's GL state changed. On desktop GL:
/// every attribute group, every client attribute group and the projection, model-view and texture
/// matrices, then the matrix mode the caller had. On FWGS's GLES renderers, which have no attribute
/// stack: the specific state [`super::gles_state`] lists, read with `glGet*` and set back. On the
/// software renderer (or a GL library missing a function) `f` just runs.
pub fn with_saved_state<R>(f: impl FnOnce() -> R) -> R {
    if renderer() == Renderer::Gles {
        let Some(gl) = super::gles_state::Real::get() else { return f() };
        let saved = super::gles_state::save(&gl, gl.generation());
        let r = f();
        saved.restore(&gl);
        return r;
    }
    let Some(g) = save_fns() else { return f() };
    let mut mode = GL_MODELVIEW as i32;
    // SAFETY: GL 1.1 functions looked up above, called with their signatures from a render
    // callback (`HUD_Redraw`) where the renderer's context is current; each push below is
    // matched by a pop after `f`, in reverse order, and `mode` is a valid out-pointer.
    unsafe {
        (g.get_integerv)(GL_MATRIX_MODE, &mut mode);
        (g.push_attrib)(GL_ALL_ATTRIB_BITS);
        (g.push_client_attrib)(GL_CLIENT_ALL_ATTRIB_BITS);
        for m in [GL_PROJECTION, GL_MODELVIEW, GL_TEXTURE] {
            (g.matrix_mode)(m);
            (g.push_matrix)();
        }
        (g.matrix_mode)(mode as u32);
    }
    let r = f();
    // SAFETY: as above: the matching pops.
    unsafe {
        for m in [GL_TEXTURE, GL_MODELVIEW, GL_PROJECTION] {
            (g.matrix_mode)(m);
            (g.pop_matrix)();
        }
        (g.pop_client_attrib)();
        (g.pop_attrib)();
        (g.matrix_mode)(mode as u32);
    }
    r
}
