//! The Specialists client game logic (TS 3.0 port). The engine loads it through `ts-client-dll`
//! (crates/client-dll), installed as cl_dlls/client.dll, client.so (i686) and client_amd64.so
//! (FWGS 64-bit). That crate holds the exports and engine callbacks and converts the engine's raw
//! pointers; this one has no `unsafe` code (see AGENTS.md).
//!
//! Modules (docs/design.md §6):
//! `input`, `hud`, `menus` (HUD/UI work package) and `view`, `camera` (third-person camera
//! controls), `events`, `prediction`, `effects` (rendering/prediction work package), `demo` (client
//! data in demos), `spectator` (observer camera, HLTV director), `studio` (the
//! TS studio model renderer). `state` holds what both sides read. `voice` is the voice status and
//! voice ban UI (`CVoiceStatus`). `relaunch` (port addition) restarts the game and reconnects
//! (docs/implementation/client-plugins.md §13.5).

// Subsystems expose API for TS features that are dead in 3.0 (round and objective modes, cash)
// or kept for parity with the SDK; docs/implementation/integration.md lists what is unused.
#![allow(dead_code)]
#![deny(unsafe_code)]

pub mod camera;
pub mod demo;
pub mod effects;
pub mod events;
pub mod ext;
pub mod gameplay;
pub mod hud;
pub mod input;
pub mod joystick;
pub mod menus;
pub mod plugins;
pub mod prediction;
pub mod relaunch;
pub mod spectator;
pub mod state;
pub mod studio;
pub mod view;
pub mod voice;
