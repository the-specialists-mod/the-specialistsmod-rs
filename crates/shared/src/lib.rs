//! Code that must behave identically on server and client: player movement, weapon tables and
//! logic, the wire protocol, and TS constants.
//!
//! Wire compatibility with TS 3.0 is a design goal (docs/design.md §2): message names, sizes,
//! registration order and payloads, events, and the `delta.lst` field usage follow the TS 3.0
//! binaries as documented in the-specialists-decomp.

// No `unsafe` here: engine callbacks go through goldsrc (see AGENTS.md).
#![deny(unsafe_code)]

pub mod bsp;
pub mod gameplay;
pub mod plugin_lib;
pub mod pm;
pub mod protocol;
pub mod text;
pub mod ts;
pub mod weapons;

pub const GAME_NAME: &std::ffi::CStr = c"The Specialists";

pub const VEC_HULL_MIN: [f32; 3] = [-16.0, -16.0, -36.0];
pub const VEC_HULL_MAX: [f32; 3] = [16.0, 16.0, 36.0];
pub const VEC_DUCK_HULL_MIN: [f32; 3] = [-16.0, -16.0, -18.0];
pub const VEC_DUCK_HULL_MAX: [f32; 3] = [16.0, 16.0, 18.0];
pub const VEC_VIEW: [f32; 3] = [0.0, 0.0, 28.0];
/// TS duck view height (SDK: 12).
pub const VEC_DUCK_VIEW: [f32; 3] = [0.0, 0.0, 16.0];
