//! Thin, safe-ish wrappers over the raw engine tables in `goldsrc-sys`.
//!
//! The `server` and `client` features select which engine table this crate wraps, so each
//! cdylib only links the half it needs.
//!
//! Together with the two `*-dll` crates, this is where the port's `unsafe` code lives: the game
//! crates (`ts-server`, `ts-client`, `shared`) are `#![deny(unsafe_code)]` and reach the engine
//! only through the safe API here. Every `unsafe` block states why it holds (`// SAFETY:`).

// Every `unsafe` block states why it holds (see AGENTS.md).
#![warn(clippy::undocumented_unsafe_blocks)]
// The wrappers take engine edict pointers (`*mut edict_t`) as entity handles and pass them to the
// engine, as the game crates do. That's the contract in `edict.rs` (pointers come from the engine,
// its edict array outlives every callback), so clippy's "safe fn derefs a raw pointer argument" is
// allowed here rather than marking every wrapper `unsafe`.
#![allow(clippy::not_unsafe_ptr_arg_deref)]
// `eng!`/`cl_eng!`/`ev!`/`efx!`/`tri!` evaluate their arguments inside the call's `unsafe` block.
// Only goldsrc and the dll crates can use them (the game crates' `deny(unsafe_code)` rejects the
// expansion), and those crates are where unsafe code is reviewed anyway.
#![allow(clippy::macro_metavars_in_unsafe)]

pub use goldsrc_sys as sys;

pub mod edict;
pub mod ext;
pub mod msg;
pub mod panic;
pub mod pmove;
pub mod stack_align;
pub mod util;

#[cfg(feature = "client")]
pub mod cl_guard;
#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod guard;
#[cfg(feature = "client")]
pub mod joystick;
#[cfg(feature = "client")]
pub mod platform;
#[cfg(feature = "server")]
pub mod server;

/// Wrapper for engine-owned globals. The engine runs every game-library callback on one
/// thread, so plain `static` storage behind this is sound as long as nothing spawns threads
/// that touch it.
pub struct Global<T>(pub T);
// SAFETY: only accessed from the engine's single game thread (see above).
unsafe impl<T> Sync for Global<T> {}

/// Exports a map entity classname for the server library. GoldSrc creates map entities by
/// looking up an export named after the classname and calling it with the new edict's
/// `entvars_t`, so every placeable class needs one. `#[no_mangle]` is an unsafe attribute in
/// Rust 2024, so this macro carries the only `allow(unsafe_code)` the game crates see; the
/// generated function itself is safe and just calls `$create`.
///
/// On i686 Linux the generated function is a stack-realigning trampoline (issue #199,
/// [`stack_align`]): old callers (AMX Mod X 1.8.2) may call with `esp` aligned to only
/// 4 bytes, crashing Rust's SSE spills.
#[macro_export]
macro_rules! export_entity_class {
    ($class:ident, $create:expr) => {
        // -- i686 Linux: body function + naked trampoline (issue #199) ---
        //
        // Both functions are inside a `const _: () = { ... }` so the body's fixed name `_body`
        // cannot collide across invocations or with existing modules (`soundent`, `shrapnel`).
        // `#[unsafe(no_mangle)]` on the trampoline gives it the classname symbol the engine
        // resolves by `dlsym`.
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        #[allow(unsafe_code)]
        const _: () = {
            #[inline(never)]
            extern "C" fn _body(pev: *mut $crate::sys::entvars_t) {
                ($create)(pev)
            }

            #[allow(non_snake_case)]
            #[unsafe(no_mangle)]
            #[unsafe(naked)]
            pub unsafe extern "C" fn $class(_pev: *mut $crate::sys::entvars_t) {
                $crate::_stack_align_trampoline_entity!(_body)
            }
        };

        // -- every other target: the original safe export ---
        #[cfg(not(all(target_os = "linux", target_arch = "x86")))]
        #[allow(unsafe_code)]
        #[allow(non_snake_case)]
        #[unsafe(no_mangle)]
        pub extern "C" fn $class(pev: *mut $crate::sys::entvars_t) {
            ($create)(pev)
        }
    };
}
