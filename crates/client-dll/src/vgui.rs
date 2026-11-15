//! The client's VGUI2 interface for Steam GoldSrc: the `ClientFactory` and `CreateInterface`
//! exports and an `IClientVGUI` ("VClientVGUI001") object whose `UseVGUI1()` returns false.
//!
//! Steam's engine has two ways to draw the 3D view. If the client exposes no `IClientVGUI`, it
//! treats the client as a VGUI1 client: while the game UI is hidden it paints only the VGUI1 root
//! panel, and the client's VGUI1 viewport is expected to call `VGui_ViewportPaintBackground` from
//! its `paintBackground` (TS 3.0's viewport does, `0x1005b3c0`, as does the SDK's
//! `TeamFortressViewport`). The engine draws the world and calls `HUD_Redraw` from there. This port
//! has no VGUI1 viewport (that is C++ ABI), so without this interface nothing draws the world on
//! Steam: the screen stays black with the last loading dialog on it (issue #62). With
//! `UseVGUI1() == false` the engine paints the viewport background itself every frame, as it does
//! for Counter-Strike's VGUI2 client.
//!
//! A VGUI1 client also gets `IN_DeactivateMouse`/`IN_ActivateMouse` whenever the game UI takes or
//! releases input; a VGUI2 client doesn't, and is told through `HideClientUI`/`ActivateClientUI`
//! instead, so those two forward to the same mouse calls.
//!
//! The engine only looks for `VClientVGUI001` when the client also exports `ClientFactory` (the
//! `cldll_func_t` slot after `HUD_GetPlayerTeam`). FWGS calls neither export and draws the world
//! itself, so this changes nothing there.
//!
//! `IClientVGUI` is a C++ interface, so the object is a vtable pointer laid out by hand: the
//! Itanium ABI (Linux) has two destructor slots and passes `this` as the first argument; MSVC has
//! one (the scalar deleting destructor) and uses `thiscall` on 32-bit x86. The engine never
//! deletes the object, which is a static.

use std::ffi::{CStr, c_char, c_int, c_void};

/// `INTERFACEVERSION_CLIENTVGUI`.
const CLIENT_VGUI_VERSION: &CStr = c"VClientVGUI001";
/// `IFACE_OK` / `IFACE_FAILED` (`public/interface.h`).
const IFACE_OK: c_int = 0;
const IFACE_FAILED: c_int = 1;

/// Defines a method with the C++ member-function ABI of the target.
macro_rules! method {
    ($(fn $name:ident($($a:ident: $t:ty),*) $(-> $r:ty)? $body:block)*) => {$(
        #[cfg(all(windows, target_arch = "x86"))]
        unsafe extern "thiscall" fn $name(_this: *mut Iface, $($a: $t),*) $(-> $r)? $body
        #[cfg(not(all(windows, target_arch = "x86")))]
        unsafe extern "C" fn $name(_this: *mut Iface, $($a: $t),*) $(-> $r)? $body
    )*};
}

method! {
    fn destructor() {}
    // `Initialize(CreateInterfaceFn *factories, int count)`: nothing to look up.
    fn initialize(_factories: *mut c_void, _count: c_int) {}
    fn start() {}
    // `SetParent(VPANEL parent)`: the port has no VGUI2 panels to parent.
    fn set_parent(_parent: u32) {}
    fn use_vgui1() -> bool {
        false
    }
    fn hide_score_board() {}
    fn hide_all_vgui_menu() {}
    // `CBaseUI::HideGameUI` calls this when the game UI closes over a running game.
    fn activate_client_ui() {
        ts_client::input::activate_mouse();
    }
    // `CBaseUI::ActivateGameUI` calls this when the game UI (menu or console) opens.
    fn hide_client_ui() {
        ts_client::input::deactivate_mouse();
    }
    fn shutdown() {}
}

/// Destructor slots: MSVC's scalar deleting destructor, or the Itanium ABI's complete-object and
/// deleting destructors.
#[cfg(windows)]
const DTORS: usize = 1;
#[cfg(not(windows))]
const DTORS: usize = 2;

/// The vtable, as untyped function pointers in declaration order.
#[repr(transparent)]
#[allow(dead_code, reason = "only the engine reads the slots, through the object's vtable pointer")]
struct VTable([*const c_void; DTORS + 9]);
// SAFETY: the table is immutable and holds only function addresses.
unsafe impl Sync for VTable {}

static VTABLE: VTable = VTable(vtable());

const fn vtable() -> [*const c_void; DTORS + 9] {
    let methods = [
        initialize as *const c_void,
        start as *const c_void,
        set_parent as *const c_void,
        use_vgui1 as *const c_void,
        hide_score_board as *const c_void,
        hide_all_vgui_menu as *const c_void,
        activate_client_ui as *const c_void,
        hide_client_ui as *const c_void,
        shutdown as *const c_void,
    ];
    let mut t = [destructor as *const c_void; DTORS + 9];
    let mut i = 0;
    while i < methods.len() {
        t[DTORS + i] = methods[i];
        i += 1;
    }
    t
}

/// The interface object: a C++ object with no data members besides its vtable pointer.
#[repr(C)]
struct Iface {
    vtable: &'static VTable,
}

static CLIENT_VGUI: Iface = Iface { vtable: &VTABLE };

goldsrc::stack_aligned! {
    /// `CreateInterface(name, returnCode)`: the client's interface factory. Only `VClientVGUI001`
    /// is provided; GameUI also asks for `GameClientExports001`, which TS doesn't expose either.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn CreateInterface(name: *const c_char, ret: *mut c_int) -> *mut c_void {
        // SAFETY: the engine passes null or a NUL-terminated interface name valid for this call.
        let found = !name.is_null() && unsafe { CStr::from_ptr(name) } == CLIENT_VGUI_VERSION;
        // SAFETY: `ret` is null or the caller's out-pointer.
        if let Some(r) = unsafe { ret.as_mut() } {
            *r = if found { IFACE_OK } else { IFACE_FAILED };
        }
        // The engine only calls methods on the object, which has no mutable state.
        if found { (&raw const CLIENT_VGUI).cast_mut().cast() } else { std::ptr::null_mut() }
    }
}

goldsrc::stack_aligned! {
    /// `ClientFactory()` (`cldll_func_t::pClientFactory`, SDK `engine/APIProxy.h`): returns the
    /// factory above. Steam's engine reads the client's `CreateInterface` only if this export exists.
    #[unsafe(no_mangle)]
    pub extern "C" fn ClientFactory() -> *mut c_void {
        CreateInterface as *mut c_void
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_returns_only_the_client_vgui_interface() {
        let mut rc = -1;
        // SAFETY: valid NUL-terminated names and a valid out-pointer.
        let p = unsafe { CreateInterface(c"VClientVGUI001".as_ptr(), &mut rc) };
        assert!(!p.is_null());
        assert_eq!(rc, IFACE_OK);
        // SAFETY: as above.
        let q = unsafe { CreateInterface(c"GameClientExports001".as_ptr(), &mut rc) };
        assert!(q.is_null());
        assert_eq!(rc, IFACE_FAILED);
        // SAFETY: null name and out-pointer are tolerated.
        assert!(unsafe { CreateInterface(std::ptr::null(), std::ptr::null_mut()) }.is_null());
    }

    #[test]
    fn use_vgui1_slot_says_no() {
        // `UseVGUI1` is the slot after the destructor(s), `Initialize`, `Start` and `SetParent`.
        let slot = if cfg!(windows) { 4 } else { 5 };
        assert_eq!(CLIENT_VGUI.vtable.0[slot], use_vgui1 as *const c_void);
        // SAFETY: the method ignores `this`.
        assert!(!unsafe { use_vgui1(std::ptr::null_mut()) });
    }
}
