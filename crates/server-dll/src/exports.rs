//! The server library's exports outside the engine's function tables, so the export lists match
//! TS's (`nm -D --defined-only ts_i386.so`, `objdump -p mp.dll`), plus the port's plugin API:
//!
//! * `Server_GetBlendingInterface` (both builds): TS's dead server studio blending interface
//!   (`ts_server::studio`).
//! * `CreateInterface` (both builds): the tier1 interface factory. `mp.dll`'s has nothing
//!   registered; the port's knows one name, the plugin API (`ts_plugin_api::INTERFACE_NAME`), and
//!   is also exported by the Linux libraries, where TS has no `CreateInterface` (port addition:
//!   the one deliberate difference from `ts_i386.so`'s export list).
//! * `anglemod`, `Length`, `Q_log2` (`ts_i386.so` only): `pm_math.c` helpers GCC exported.
//!   Nothing calls them.

use goldsrc::sys::*;
use std::ffi::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering::Relaxed};

/// `svBlending` (TS .data 0x104ea8, objdump numbering): version 1 and TS's `SV_StudioSetupBones`.
/// The engine only reads it, and drops it again because the export returns 0.
static SV_BLENDING: sv_blending_interface_t =
    sv_blending_interface_t { version: SV_BLENDING_INTERFACE_VERSION as c_int, SV_StudioSetupBones: Some(sv_studio_setup_bones) };

/// TS's `IEngineStudio` copy of the engine's `server_studio_api_t` (unused in TS too).
static ENGINE_STUDIO: Mutex<Option<server_studio_api_t>> = Mutex::new(None);
/// TS's `g_pRotationMatrix` (`float (*)[3][4]`) and `g_pBoneTransform`
/// (`float (*)[MAXSTUDIOBONES][3][4]`): the engine's bone arrays, stored and never read.
static ROTATION_MATRIX: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static BONE_TRANSFORM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

goldsrc::stack_aligned! {
    /// `Server_GetBlendingInterface` (TS 0xa3f80, mp.dll 0x10008850). For version 1 it hands the
    /// engine `svBlending`, copies the studio API and stores the two matrix pointers; it returns 0
    /// for every version, so FWGS and (Re)HLDS go back to their own bone setup (ReHLDS also prints
    /// "Couldn't get server .dll studio model blending interface. Version mismatch?" in developer
    /// mode).
    ///
    /// Guard: TS writes through `ppinterface` and reads `pstudio` without a NULL check; the port
    /// skips a NULL one.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Server_GetBlendingInterface(
        version: c_int,
        ppinterface: *mut *mut sv_blending_interface_t,
        pstudio: *const server_studio_api_t,
        rotationmatrix: *mut c_void,
        bonetransform: *mut c_void,
    ) -> c_int {
        if ts_server::studio::blending_interface_accepts(version) {
            if !ppinterface.is_null() {
                // SAFETY: the engine passes a writable out-pointer for the interface. The static is
                // only read through it (both engines call `SV_StudioSetupBones` or drop the pointer).
                unsafe { *ppinterface = (&raw const SV_BLENDING).cast_mut() };
            }
            if !pstudio.is_null() {
                // SAFETY: the engine passes its static `server_studio_api_t`.
                let api = unsafe { *pstudio };
                *ENGINE_STUDIO.lock().unwrap_or_else(|e| e.into_inner()) = Some(api);
            }
            ROTATION_MATRIX.store(rotationmatrix, Relaxed);
            BONE_TRANSFORM.store(bonetransform, Relaxed);
        }
        ts_server::studio::blending_interface_result(version)
    }
}

goldsrc::stack_aligned! {
    /// `SV_StudioSetupBones` (TS 0xa3fdc, mp.dll 0x100088a0), `svBlending`'s function. TS ignores
    /// every argument.
    unsafe extern "C" fn sv_studio_setup_bones(
        _model: *mut model_s,
        _frame: f32,
        _sequence: c_int,
        _angles: *const vec_t,
        _origin: *const vec_t,
        _controller: *const byte,
        _blending: *const byte,
        _bone: c_int,
        _edict: *const edict_t,
    ) {
        ts_server::studio::sv_studio_setup_bones();
    }
}

/// `IFACE_OK` / `IFACE_FAILED` (tier1 interface.h).
const IFACE_OK: c_int = 0;
const IFACE_FAILED: c_int = 1;

goldsrc::stack_aligned! {
    /// `CreateInterface(name, returnCode)` (mp.dll 0x10028f70): tier1's factory. It walks
    /// `InterfaceReg::s_pInterfaceRegs` (0x100ae738), but nothing in mp.dll registers an interface
    /// (that is the only reference to the list head), so in TS every lookup fails: `IFACE_FAILED`
    /// and NULL. `ts_i386.so` doesn't have it.
    ///
    /// Port addition: `ts_plugin_api::INTERFACE_NAME` gives the plugin API table
    /// ([`crate::plugin_api::table`]) and `IFACE_OK`, on every platform. Guard: a NULL name fails
    /// (TS's `strcmp` loop never reads it, having no entries).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn CreateInterface(name: *const std::ffi::c_char, return_code: *mut c_int) -> *mut c_void {
        // SAFETY: a non-NULL name is the caller's NUL-terminated string, read for this call only.
        let found = !name.is_null() && ts_server::plugin_api::interface(unsafe { std::ffi::CStr::from_ptr(name) });
        if !return_code.is_null() {
            // SAFETY: a non-NULL return code is the caller's writable int (TS checks NULL too).
            unsafe { *return_code = if found { IFACE_OK } else { IFACE_FAILED } };
        }
        if found { std::ptr::from_ref(crate::plugin_api::table()).cast_mut().cast() } else { std::ptr::null_mut() }
    }
}

#[cfg(not(windows))]
goldsrc::stack_aligned! {
    /// `anglemod` (TS 0xfb3d8, `shared::pm::anglemod`).
    #[unsafe(no_mangle)]
    pub extern "C" fn anglemod(a: f32) -> f32 {
        shared::pm::anglemod(a)
    }
}

#[cfg(not(windows))]
goldsrc::stack_aligned! {
    /// `Length` (TS 0xfbcbc, `shared::pm::length`).
    ///
    /// Guard: TS reads `v[0..3]` without a NULL check; the port returns 0 for NULL.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn Length(v: *const vec_t) -> f32 {
        if v.is_null() {
            return 0.0;
        }
        // SAFETY: a non-NULL argument is a `vec3_t`, three floats, as in TS's signature.
        let a = unsafe { *v.cast::<[f32; 3]>() };
        shared::pm::length(a)
    }
}

#[cfg(not(windows))]
goldsrc::stack_aligned! {
    /// `Q_log2` (TS 0xfbd8c, `shared::pm::q_log2`).
    #[unsafe(no_mangle)]
    pub extern "C" fn Q_log2(val: c_int) -> c_int {
        shared::pm::q_log2(val)
    }
}
