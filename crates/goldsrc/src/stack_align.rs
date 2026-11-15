//! Stack-alignment trampolines for i686 Linux.
//!
//! Rust's `i686-unknown-linux-gnu` target assumes a 16-byte aligned stack at every function
//! entry (it spills SSE registers with `movaps`/`movdqa`), but the i386 System V ABI only
//! guarantees 4 bytes, and old callers deliver no more. AMX Mod X 1.8.2's Linux core calls
//! into the game library through the `DLL_FUNCTIONS` table with `esp` = 8 mod 16, crashing
//! on the first SSE spill. TS's own `ts_i386.so` (GCC, no SSE) never cared.
//!
//! The macros in this module generate a naked trampoline for every `extern "C"` function the
//! engine or a plugin may call. The trampoline saves `ebp`, realigns `esp` to 16, copies the
//! caller's stack arguments to the new frame, calls the real implementation, and restores the
//! original frame on return. On every other target the macros are identity: they emit the
//! function as written.
//!
//! # Design
//!
//! Each trampoline copies a fixed 48 bytes (12 dwords) of arguments, enough for every GoldSrc
//! callback (the largest is `HUD_TempEntUpdate` at 40 bytes). The copy uses `eax` as a
//! temporary, which is caller-saved and not yet carrying a return value. After `call`, the
//! return value in `eax` (integer/pointer) or `ST(0)` (float) is untouched by the
//! `mov esp, ebp; pop ebp; ret` epilogue.
//!
//! The body function is placed inside a module named after the original function (modules
//! occupy the type namespace, functions the value namespace, so they coexist). A
//! `use super::*;` import gives the body access to everything visible at the call site.
//!
//! # Scope
//!
//! Only `target_arch = "x86"` + `target_os = "linux"` is affected. Windows i686 already
//! assumes 4-byte alignment (`i686-pc-windows-gnu`'s ABI), and x86_64 always has 16-byte
//! alignment. The trampolines are `cfg`-gated accordingly.
//!
//! See also: GitHub issue #199.

/// Emits the `naked_asm!` body for a stack-realigning trampoline.
///
/// The trampoline copies 48 bytes (12 dwords) of the caller's arguments from the misaligned
/// frame to a 16-byte aligned one, calls `$mod::body`, and returns. 48 is a multiple of 16
/// so `esp` stays aligned after the `sub`, and after `call` pushes a 4-byte return address
/// the callee sees `esp + 4 = 0 mod 16` (i386 SysV ABI requirement).
///
/// Internal: used by [`stack_aligned!`].
#[macro_export]
#[doc(hidden)]
macro_rules! _stack_align_trampoline {
    ($mod_name:ident) => {
        // Intel syntax is the default for Rust's asm! / naked_asm! on x86.
        ::core::arch::naked_asm!(
            // Save the caller's frame pointer and capture the (potentially misaligned) esp.
            "push ebp",
            "mov ebp, esp",
            // Align esp down to the nearest 16-byte boundary.
            "and esp, -16",
            // Allocate 48 bytes for the copied arguments (48 = 3 * 16, keeps alignment).
            "sub esp, 48",
            // Copy 12 dwords from the caller's argument area [ebp+8..ebp+56] to [esp..esp+48].
            "mov eax, [ebp+8]",  "mov [esp], eax",
            "mov eax, [ebp+12]", "mov [esp+4], eax",
            "mov eax, [ebp+16]", "mov [esp+8], eax",
            "mov eax, [ebp+20]", "mov [esp+12], eax",
            "mov eax, [ebp+24]", "mov [esp+16], eax",
            "mov eax, [ebp+28]", "mov [esp+20], eax",
            "mov eax, [ebp+32]", "mov [esp+24], eax",
            "mov eax, [ebp+36]", "mov [esp+28], eax",
            "mov eax, [ebp+40]", "mov [esp+32], eax",
            "mov eax, [ebp+44]", "mov [esp+36], eax",
            "mov eax, [ebp+48]", "mov [esp+40], eax",
            "mov eax, [ebp+52]", "mov [esp+44], eax",
            // Call the real implementation on the aligned stack.
            "call {body}",
            // Restore esp from the saved frame pointer and return. The return value
            // (eax / edx:eax for integers, ST(0) for floats) is not touched.
            "mov esp, ebp",
            "pop ebp",
            "ret",
            body = sym $mod_name::body,
        )
    };
}

/// Wraps an `extern "C"` function with a stack-realigning trampoline on i686 Linux.
///
/// On `target_arch = "x86"` + `target_os = "linux"`, the generated naked trampoline saves
/// `ebp`, realigns `esp` to 16 bytes, copies the caller's stack arguments to the aligned
/// frame, calls the real implementation, and restores the original frame. On all other
/// targets the function is emitted unchanged.
///
/// Both `unsafe` and safe `extern "C"` (and `extern "system"`) functions are accepted.
/// Attributes (`#[unsafe(no_mangle)]`, `#[allow(...)]`, etc.) go inside the macro and are
/// forwarded to the trampoline (or to the plain function on other targets). Platform `#[cfg]`
/// attributes belong outside the invocation.
///
/// # Example
///
/// ```ignore
/// goldsrc::stack_aligned! {
///     #[unsafe(no_mangle)]
///     pub unsafe extern "C" fn GetEntityAPI2(
///         table: *mut DLL_FUNCTIONS,
///         ver: *mut c_int,
///     ) -> c_int {
///         // body
///     }
/// }
/// ```
///
/// On i686 Linux, a module `GetEntityAPI2` (type namespace) is created alongside the
/// function (value namespace) to scope the real implementation as `GetEntityAPI2::body`.
#[macro_export]
macro_rules! stack_aligned {
    // ---- unsafe extern "..." variant ---------------------------------------------------------------
    (
        $(#[$attr:meta])*
        $vis:vis unsafe extern $abi:literal fn $name:ident($($pname:ident: $pty:ty),* $(,)?) $(-> $ret:ty)? {
            $($body:tt)*
        }
    ) => {
        // -- i686 Linux: body in a same-named module + naked trampoline ---
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            #[inline(never)]
            #[allow(clippy::too_many_arguments)]
            pub(super) unsafe extern $abi fn body($($pname: $pty),*) $(-> $ret)? {
                $($body)*
            }
        }
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        $(#[$attr])*
        #[unsafe(naked)]
        $vis unsafe extern $abi fn $name($($pname: $pty),*) $(-> $ret)? {
            $crate::_stack_align_trampoline!($name)
        }

        // -- every other target: the plain function ---
        #[cfg(not(all(target_os = "linux", target_arch = "x86")))]
        $(#[$attr])*
        $vis unsafe extern $abi fn $name($($pname: $pty),*) $(-> $ret)? {
            $($body)*
        }
    };

    // ---- safe extern "..." variant -----------------------------------------------------------------
    (
        $(#[$attr:meta])*
        $vis:vis extern $abi:literal fn $name:ident($($pname:ident: $pty:ty),* $(,)?) $(-> $ret:ty)? {
            $($body:tt)*
        }
    ) => {
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            #[inline(never)]
            #[allow(clippy::too_many_arguments)]
            pub(super) extern $abi fn body($($pname: $pty),*) $(-> $ret)? {
                $($body)*
            }
        }
        #[cfg(all(target_os = "linux", target_arch = "x86"))]
        $(#[$attr])*
        #[unsafe(naked)]
        $vis extern $abi fn $name($($pname: $pty),*) $(-> $ret)? {
            $crate::_stack_align_trampoline!($name)
        }

        #[cfg(not(all(target_os = "linux", target_arch = "x86")))]
        $(#[$attr])*
        $vis extern $abi fn $name($($pname: $pty),*) $(-> $ret)? {
            $($body)*
        }
    };
}

/// Like [`_stack_align_trampoline!`] but takes a bare function name instead of `$mod::body`.
///
/// Used by [`export_entity_class!`] where the body function is inside an anonymous const
/// block rather than a named module.
#[macro_export]
#[doc(hidden)]
macro_rules! _stack_align_trampoline_entity {
    ($body_fn:ident) => {
        ::core::arch::naked_asm!(
            "push ebp",
            "mov ebp, esp",
            "and esp, -16",
            "sub esp, 48",
            "mov eax, [ebp+8]",  "mov [esp], eax",
            "mov eax, [ebp+12]", "mov [esp+4], eax",
            "mov eax, [ebp+16]", "mov [esp+8], eax",
            "mov eax, [ebp+20]", "mov [esp+12], eax",
            "mov eax, [ebp+24]", "mov [esp+16], eax",
            "mov eax, [ebp+28]", "mov [esp+20], eax",
            "mov eax, [ebp+32]", "mov [esp+24], eax",
            "mov eax, [ebp+36]", "mov [esp+28], eax",
            "mov eax, [ebp+40]", "mov [esp+32], eax",
            "mov eax, [ebp+44]", "mov [esp+36], eax",
            "mov eax, [ebp+48]", "mov [esp+40], eax",
            "mov eax, [ebp+52]", "mov [esp+44], eax",
            "call {body}",
            "mov esp, ebp",
            "pop ebp",
            "ret",
            body = sym $body_fn,
        )
    };
}
