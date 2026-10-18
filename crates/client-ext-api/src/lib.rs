//! The C ABI the client library offers client extensions: a port addition, TS has no such API
//! (issue #293, docs/implementation/client-ext-api.md).
//!
//! A client extension is a native library the port's `client.dll` / `client.so` loads into the
//! game process: the first is the webview overlay (`the-specialists-mod/tsmod-plugin-webview-rs`).
//! It is the client's counterpart of the server's `tsmod_*` plugins (`ts-plugin-api`), and is
//! built the same way: one exported entry point, a table of port functions it may call, and a
//! descriptor of optional callbacks it fills in.
//!
//! # Loading
//! At `Initialize` the client looks at every file in its own `cl_dlls/` directory named
//! `tsmod_<name>` plus the platform suffix ([`library_suffix`]: `.dll`, `_amd64.dll`,
//! `_i386.so`, `_amd64.so`, the server plugins' naming), skips the ones built for another
//! platform, and asks the **approval gate** about each of the rest. Nothing is mapped that the
//! gate doesn't approve (docs/implementation/client-plugins.md §5, §9): the player approves a
//! plugin's exact bytes. An approved library's exported [`LOAD_SYMBOL`] ([`LoadFn`]) is called
//! once. The extension checks the table's [`ClientExtApi::size`] and [`ClientExtApi::version`],
//! fills the [`ClientExt`] it is given (its `size` and `api_version` are checked after the call)
//! and returns [`OK`]. Anything else skips the library with a console line. Libraries stay
//! loaded for the life of the process; GoldSrc has no way to remove a console command, and
//! unloading native code that may have handed out pointers isn't safe.
//!
//! # Versions
//! * [`VERSION`] is the major version and must match exactly, both ways ([`ClientExtApi::version`]
//!   and [`ClientExt::api_version`]); so must both structs' sizes. A break bumps it.
//! * [`MINOR`] counts compatible additions. Both structs end in reserved slots
//!   ([`ClientExtApi::reserved`], [`ClientExt::reserved`]), which keep the sizes fixed. A minor
//!   version gives reserved slots a type and a meaning; nothing else moves. An extension reads
//!   [`ClientExtApi::minor`] before calling a function added after minor 0 (an older client leaves
//!   it NULL), and may fill callbacks a newer minor defines: an older client never calls them.
//!   Minor 1 added the `TSExt` channel messages (issue #294) and the interception hooks
//!   `message_pre`, `command_pre`, `server_cmd_pre` with [`ClientExtApi::release`] (issue #295).
//!   Minor 2 added [`local::DUCKING`], dropped dead players from `visible_players` and saves the
//!   GLES state around `redraw` (issue #306), and HUD control and menu takeover (issue #314).
//!
//! # HUD control and menus (minor 2)
//! * **Hiding** ([`ClientExtApi::hud_set_hidden`]): each extension has its own mask of [`hud`]
//!   elements; an element is hidden while any extension that isn't suspended hides it. A hidden
//!   element only stops drawing: the port keeps its state (and keeps working out where it would
//!   draw, [`ClientExtApi::hud_elements`]), so showing it again shows it as it would be now.
//!   Suspension shows an extension's elements again.
//! * **Takeover:** a [`verdict::BLOCK`] on a `ShowMenu`, `VGUIMenu` (the team menu, the map
//!   briefing, the MOTD window) or `MOTD` takes the menu over: the port doesn't show it, and the
//!   extension that blocked it may answer it once with [`ClientExtApi::menu_reply`], which sends
//!   exactly what the port's menu would have (`menuselect N`, `jointeam N`), and only a choice
//!   the menu offered. The takeover ends when it is answered, when a newer menu of its kind comes,
//!   when the server closes it (a `ShowMenu` without keys), when its time runs out (`ShowMenu`'s
//!   display time), on `ResetHUD` and when the connection ends ([`ClientExt::menu_closed`]).
//! * **Fail open:** a taken-over menu comes back as the port's own when its extension is
//!   suspended, or when its [`ClientExt::menu_shown`] (asked every frame) says it no longer shows
//!   it (its page closed or crashed). An extension without `menu_shown` (built for minor 1) is
//!   never asked: its block stays a block, as in minor 1, until it is suspended.
//!
//! # Interception (minor 1)
//! * **What reaches the hooks:** `message_pre` gets every user message of TS 3.0's the port hooks
//!   but `SrvSett` (none of the port's own, `TSExt` among them); `command_pre` every client command
//!   the port registered; and
//!   `server_cmd_pre` every command the port's own code sends to the server (weapon selection,
//!   menu answers, buying, ...). Server `stufftext` (`svc_stufftext`) is the engine's: no client
//!   library sees it, so no hook does.
//! * **What a verdict can do** is in each call's `allow` ([`allow`] bits). Messages that keep the
//!   HUD and prediction in step with the server, effects meant to hinder the player (fades,
//!   shakes, blur, damage, slow motion), input buttons (`+attack`, `+use`, every `+`/`-`
//!   command), extensions' own commands, the dev aids, and the commands and replies of the plugin
//!   approval path (`cl_plugins*`, `tsrs_*`, `tspkg`, client-plugins.md §7.3) are observe-only:
//!   `allow` is 0 and the verdict is ignored.
//! * **Order:** extensions in load order. Each sees the payload as the ones before it rewrote it;
//!   the first block ends the run (later extensions don't see it). A [`verdict::DEFER`] pauses the
//!   run at that extension until it calls [`ClientExtApi::release`] (or [`HOLD_MS`] pass: then it
//!   passes); the run then goes on with the next extension.
//! * **State and display:** the port applies a message's state at once, whatever the verdict,
//!   and blocks, rewrites or holds only its display: a blocked or held `DeathMsg` still marks the
//!   victim dead on the scoreboard and still raises [`event::DEATH`]. Messages of one kind are
//!   displayed in arrival order: one held holds back the later ones of its kind (at most
//!   [`HOLD_QUEUE`]; past that the oldest passes).
//! * **Replacements** must decode as the message and keep who it names (a `SayText`'s sender, a
//!   `DeathMsg`'s killer and victim, a `TSPAward`'s player); otherwise they count as a block,
//!   with a console warning.
//! * **Suspension** ends an extension's holds (they pass) and skips it in every run.
//!
//! # Rules
//! * The port calls the extension only on the engine's thread, from inside a client export, and
//!   the extension calls the table only from there too (its callbacks and commands qualify).
//! * Strings are NUL-terminated and valid for the call; copy what you keep. Functions that write
//!   a string take a buffer and its length, write as much as fits (always NUL-terminated) and
//!   return the full length, so a short buffer can be retried.
//! * Callbacks run in load order (library file name order).
//! * **Suspended** extensions (a server's `block` with `suspend`, client-plugins.md §13.3) get no
//!   callback and no command until [`ClientExt::resume`], except [`ClientExt::shutdown`].
//! * The game state is **what the player can see**: the HUD's own values, the scoreboard, the
//!   view, and players in view. Nothing reveals a player the screen doesn't show (no origins of
//!   players behind walls, no traces from anywhere but the eye through players).

#![no_std]

use core::ffi::{CStr, c_char, c_int, c_void};

/// [`ClientExtApi::version`] of this definition (and what [`ClientExt::api_version`] must be).
pub const VERSION: u32 = 1;
/// [`ClientExtApi::minor`]: the compatible additions this definition has (see the crate docs).
/// 1: `TSExt` channels ([`ClientExtApi::claim_channel`], [`ClientExt::user_message`]) and the
/// interception hooks ([`ClientExt::message_pre`], [`ClientExt::command_pre`],
/// [`ClientExt::server_cmd_pre`], [`ClientExtApi::release`]).
/// 2: [`local::DUCKING`], dead players left out of [`ClientExtApi::visible_players`], GLES state
/// saved around [`ClientExt::redraw`] (issue #306); HUD control ([`ClientExtApi::hud_set_hidden`],
/// [`ClientExtApi::hud_elements`]) and menu takeover ([`ClientExtApi::menu_reply`],
/// [`ClientExt::menu_shown`], [`ClientExt::menu_closed`]) (issue #314).
pub const MINOR: u32 = 2;
/// The entry point every client extension exports ([`LoadFn`]).
pub const LOAD_SYMBOL: &CStr = c"TSRS_ClientExtLoad";
/// The prefix of the libraries the client loads from `cl_dlls/`.
pub const LIBRARY_PREFIX: &str = "tsmod_";
/// The number of [`ClientExtApi::reserved`] slots.
pub const API_RESERVED: usize = 27;
/// The number of [`ClientExt::reserved`] slots.
pub const EXT_RESERVED: usize = 10;

/// The file name suffix of an extension library for a platform and architecture, the same as the
/// server's `tsmod_*` plugins: `.dll`, `_amd64.dll`, `_i386.so`, `_amd64.so`. (The client library
/// itself is `client.so` on 32-bit Linux; the `_i386` keeps the four names apart in one
/// `cl_dlls/`.)
pub const fn library_suffix(windows: bool, x86_64: bool) -> &'static str {
    match (windows, x86_64) {
        (true, false) => ".dll",
        (true, true) => "_amd64.dll",
        (false, false) => "_i386.so",
        (false, true) => "_amd64.so",
    }
}

/// [`library_suffix`] of the platform this crate is built for.
pub const LIBRARY_SUFFIX: &str = library_suffix(cfg!(windows), cfg!(target_arch = "x86_64"));

/// Done.
pub const OK: c_int = 0;
/// No such player, or nothing to report (no local player, no map).
pub const NOT_FOUND: c_int = -1;
/// An argument is out of range or malformed; nothing happened.
pub const INVALID: c_int = -2;
/// The engine lacks what the call needs (no triangle or event API, no GL).
pub const UNAVAILABLE: c_int = -3;
/// The extension is suspended; nothing happened.
pub const SUSPENDED: c_int = -4;
/// A name is taken (a command or cvar); nothing changed.
pub const EXISTS: c_int = -6;
/// [`ClientExtApi::verified_spawn`]: the approval doesn't cover that file as it is now.
pub const REFUSED: c_int = -7;
/// [`ClientExtApi::verified_spawn`]: the system couldn't start it.
pub const FAILED: c_int = -8;

/// An extension, as [`LoadInfo::id`] gives it: passed back to the functions that act for it
/// (its log prefix, data directory, commands, helper processes). Never 0.
pub type ExtId = c_int;

/// `TSRS_ClientExtLoad(api, api_size, info, out, out_size)`: called once, at the client's
/// `Initialize` (or, for a plugin approved later, when it is approved, client-plugins.md §9).
/// `api` is the table (`api_size` = its [`ClientExtApi::size`]); `info` says who the extension
/// is (valid for the call; keep [`LoadInfo::id`]); `out` is a zeroed [`ClientExt`] of `out_size`
/// bytes whose `size` the client has set. Fill it and return [`OK`]; any other value refuses to
/// load. Don't call the table here except for `print`: the engine isn't initialised yet (register
/// commands and cvars in [`ClientExt::init`]).
pub type LoadFn =
    unsafe extern "C" fn(api: *const ClientExtApi, api_size: u32, info: *const LoadInfo, out: *mut ClientExt, out_size: u32) -> c_int;

/// A console command an extension registers ([`ClientExtApi::add_command`]). It reads its
/// arguments with [`ClientExtApi::cmd_argc`] and [`ClientExtApi::cmd_argv`].
pub type CommandFn = extern "C" fn();

/// A reserved slot: NULL until a [`MINOR`] version defines it.
pub type Reserved = Option<unsafe extern "C" fn()>;

/// Who the extension is, given to [`LoadFn`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoadInfo {
    /// This extension's id (> 0).
    pub id: ExtId,
    /// `<name>` of `tsmod_<name>`, NUL-terminated.
    pub name: *const c_char,
    /// The full path of the library, NUL-terminated.
    pub path: *const c_char,
}

/// An extension's description and callbacks, filled by [`LoadFn`]. Every callback is optional
/// and runs on the engine's thread.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ClientExt {
    /// `size_of::<ClientExt>()`: the client sets it before the call and checks it after.
    pub size: u32,
    /// The [`VERSION`] the extension was built for: must be the client's.
    pub api_version: u32,
    /// Display name and version (NUL-terminated, static): for the console.
    pub name: *const c_char,
    pub version: *const c_char,

    /// `HUD_Init`, after the port's HUD: register commands and cvars here. A plugin approved
    /// after start gets it at once, then [`Self::vid_init`] if a map is loaded.
    pub init: Option<extern "C" fn()>,
    /// `HUD_VidInit`: a new map or video mode; GL textures are gone.
    pub vid_init: Option<extern "C" fn()>,
    /// `HUD_Frame`, after the port's frame.
    pub frame: Option<extern "C" fn(time: f64)>,
    /// `HUD_Redraw`, after the port's HUD and menus. On desktop GL the port saves the GL state
    /// (attributes, client attributes and the three matrix stacks) before and restores it after,
    /// so the extension may change any of it. On GLES (from client minor 2) it saves and restores
    /// the state an overlay usually changes, read with `glGet*`: the enables, blending, depth,
    /// viewport, scissor, colour mask, the active texture unit and the 2D textures bound on it
    /// and on unit 0, pixel alignment, the program, buffers, framebuffer, vertex array object and
    /// attribute arrays 0..8 (GLES 2+), or the colour, client arrays and matrices (GLES 1).
    /// Anything else (uniforms, attribute pointers, texture parameters) is the extension's to put
    /// back. The software renderer has no GL.
    pub redraw: Option<extern "C" fn(time: f32, intermission: c_int)>,
    /// `HUD_Key_Event`, before the port (so Escape reaches the extension first). `binding` is the
    /// key's binding ("" for none). Non-zero swallows the key: neither the port nor the engine
    /// sees it. Mouse buttons arrive here as `K_MOUSE1`.. keys.
    pub key_event: Option<unsafe extern "C" fn(down: c_int, key: c_int, binding: *const c_char) -> c_int>,
    /// `IN_ActivateMouse` (1) / `IN_DeactivateMouse` (0).
    pub mouse_active: Option<extern "C" fn(active: c_int)>,
    /// Asked every frame: non-zero takes the mouse for a cursor, as the port's own menus do. The
    /// view then holds still, mouse motion moves the cursor ([`ClientExtApi::mouse_position`]),
    /// and clicks arrive through [`Self::key_event`].
    pub wants_cursor: Option<extern "C" fn() -> c_int>,
    /// `CL_CreateMove`, after the port built the command: a read-only copy.
    pub usercmd: Option<unsafe extern "C" fn(cmd: *const UserCmd)>,
    /// Something happened that the HUD shows ([`GameEvent`], [`event`] kinds).
    pub game_event: Option<unsafe extern "C" fn(event: *const GameEvent)>,
    /// The client suspended the extension (`reason` for the console). Nothing else is called
    /// until [`Self::resume`].
    pub suspend: Option<unsafe extern "C" fn(reason: *const c_char)>,
    pub resume: Option<extern "C" fn()>,
    /// The client connected to a server (`address` as the engine reports it: `loopback`,
    /// `1.2.3.4:27015`) or disconnected (`""`). Called at the first frame that sees the change.
    pub server_changed: Option<unsafe extern "C" fn(address: *const c_char)>,
    /// `HUD_Shutdown`. Called even while suspended.
    pub shutdown: Option<extern "C" fn()>,

    // ---- minor 1: `TSExt` channels and interception (issues #294, #295) ---------------------------
    /// A `TSExt` message on a channel this extension claimed ([`ClientExtApi::claim_channel`]):
    /// the channel's name and the payload's `len` bytes (at most [`MESSAGE_MAX`] minus the channel
    /// and its NUL), valid for the call.
    pub user_message: Option<unsafe extern "C" fn(channel: *const c_char, data: *const u8, len: c_int)>,
    /// A user message from the server, before the port handles it ([`MessagePre`]): return a
    /// [`verdict`]. For [`verdict::REPLACE`] write the new payload (the message's own wire format)
    /// to `out`, which has room for [`MESSAGE_MAX`] bytes, and its length to `*out_len`. The port
    /// takes only what [`MessagePre::allow`] allows and passes anything else.
    pub message_pre: Option<unsafe extern "C" fn(msg: *const MessagePre, out: *mut u8, out_len: *mut c_int) -> c_int>,
    /// One of the port's client commands is about to run ([`CommandPre`]): [`verdict::PASS`], or
    /// [`verdict::BLOCK`] when [`CommandPre::allow`] has it. Any other verdict passes.
    pub command_pre: Option<unsafe extern "C" fn(cmd: *const CommandPre) -> c_int>,
    /// The port is about to send a command to the server ([`CommandPre`]): a [`verdict`]. For
    /// [`verdict::REPLACE`] write the new line, NUL-terminated, to `out` (`out_cap` bytes, at
    /// least [`COMMAND_MAX`]): it must keep the command's name (`argv[0]`) and hold no `;` or line
    /// break, or it counts as a block.
    pub server_cmd_pre: Option<unsafe extern "C" fn(cmd: *const CommandPre, out: *mut c_char, out_cap: c_int) -> c_int>,

    // ---- minor 2: menu takeover (issue #314) -----------------------------------------------------
    /// Asked once a frame while this extension holds an open takeover of a [`menu`] kind (see the
    /// crate docs): non-zero while it still shows the menu to the player; 0 gives it back, and the
    /// port shows its own menu (fail open). Without it the port never asks.
    pub menu_shown: Option<extern "C" fn(kind: c_int) -> c_int>,
    /// A takeover of this extension's ended without its answer: `reason` a [`menu_closed`] code.
    pub menu_closed: Option<extern "C" fn(kind: c_int, reason: c_int)>,

    /// Callbacks a later [`MINOR`] defines; leave them NULL.
    pub reserved: [Reserved; EXT_RESERVED],
}

impl ClientExt {
    /// A descriptor with no name and no callbacks, `size` and `api_version` set.
    pub const EMPTY: ClientExt = ClientExt {
        size: size_of::<ClientExt>() as u32,
        api_version: VERSION,
        name: core::ptr::null(),
        version: core::ptr::null(),
        init: None,
        vid_init: None,
        frame: None,
        redraw: None,
        key_event: None,
        mouse_active: None,
        wants_cursor: None,
        usercmd: None,
        game_event: None,
        suspend: None,
        resume: None,
        server_changed: None,
        shutdown: None,
        user_message: None,
        message_pre: None,
        command_pre: None,
        server_cmd_pre: None,
        menu_shown: None,
        menu_closed: None,
        reserved: [None; EXT_RESERVED],
    };
}

/// The longest user message (the Steam engines' `MAX_USER_MSG_DATA`): room in
/// [`ClientExt::message_pre`]'s `out`, and the limit of a `TSExt` message.
pub const MESSAGE_MAX: usize = 192;
/// The longest channel name ([`ClientExtApi::claim_channel`]), the NUL not counted.
pub const CHANNEL_MAX: usize = 15;
/// The least room in [`ClientExt::server_cmd_pre`]'s `out`.
pub const COMMAND_MAX: usize = 256;
/// How long the port holds a deferred message for an extension before it passes, milliseconds.
pub const HOLD_MS: u32 = 1000;
/// The most messages of one kind waiting behind a held one.
pub const HOLD_QUEUE: usize = 32;

/// An interception hook's answer.
pub mod verdict {
    use core::ffi::c_int;
    /// Go on as if the extension weren't there.
    pub const PASS: c_int = 0;
    /// Don't display the message / don't run or send the command.
    pub const BLOCK: c_int = 1;
    /// Use the replacement the extension wrote to `out`.
    pub const REPLACE: c_int = 2;
    /// Hold the message's display until [`ClientExtApi::release`](super::ClientExtApi::release)
    /// with the call's token.
    pub const DEFER: c_int = 3;
}

/// What a hook's verdict may do ([`MessagePre::allow`], [`CommandPre::allow`]); 0 is
/// observe-only.
pub mod allow {
    pub const BLOCK: u32 = 1;
    pub const REPLACE: u32 = 2;
    pub const DEFER: u32 = 4;
}

/// A user message for [`ClientExt::message_pre`]. Everything is valid for the call only.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MessagePre {
    /// The message's registered name (`SayText`, `DeathMsg`), NUL-terminated.
    pub name: *const c_char,
    /// The payload as it is now (earlier extensions' replacements applied): `len` bytes.
    pub data: *const u8,
    pub len: c_int,
    /// [`allow`] bits.
    pub allow: u32,
    /// The token [`ClientExtApi::release`] takes after a [`verdict::DEFER`]; 0 without
    /// [`allow::DEFER`].
    pub token: u32,
}

/// A command for [`ClientExt::command_pre`] and [`ClientExt::server_cmd_pre`]. Everything is
/// valid for the call only.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CommandPre {
    /// The whole line, NUL-terminated (`slot3`, `weapon_5`, `menuselect 2`).
    pub line: *const c_char,
    /// Its words as the engine tokenises them: `argv[0]` is the command's name.
    pub argc: c_int,
    pub argv: *const *const c_char,
    /// [`allow`] bits: [`allow::BLOCK`] at most for client commands, and
    /// [`allow::REPLACE`] for commands to the server.
    pub allow: u32,
}

/// The command the client just built ([`ClientExt::usercmd`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UserCmd {
    /// The command's buttons ([`button`] bits, TS's map: `+attack` from a key, a mouse button or
    /// a script all look the same). What the command asks for, not what the server allows.
    pub buttons: u32,
    /// Bits set in this command and not in the previous one, and the other way round.
    pub pressed: u32,
    pub released: u32,
    /// The command's view angles (pitch, yaw, roll, degrees) and moves (units per second).
    pub viewangles: [f32; 3],
    pub forwardmove: f32,
    pub sidemove: f32,
    pub upmove: f32,
    pub impulse: c_int,
    pub weaponselect: c_int,
    /// 0 when the engine built the command without input (`CL_CreateMove`'s `active`).
    pub active: c_int,
}

/// [`UserCmd::buttons`] bits: the engine's `IN_*` with TS's use of them.
pub mod button {
    pub const ATTACK: u32 = 1;
    pub const JUMP: u32 = 2;
    pub const DUCK: u32 = 4;
    pub const FORWARD: u32 = 8;
    pub const BACK: u32 = 16;
    pub const USE: u32 = 32;
    /// TS's `+alt2` (the engine's `IN_CANCEL` bit).
    pub const ALT2: u32 = 64;
    pub const LEFT: u32 = 128;
    pub const RIGHT: u32 = 256;
    pub const MOVELEFT: u32 = 512;
    pub const MOVERIGHT: u32 = 1024;
    pub const ATTACK2: u32 = 2048;
    pub const RELOAD: u32 = 8192;
    /// TS's `+alt1`, the stunt.
    pub const ALT1: u32 = 16384;
    pub const SCORE: u32 = 32768;
}

/// Bytes of the fixed strings in the structs below, the NUL included.
pub const NAME_LEN: usize = 32;
/// Bytes of [`ViewInfo::map`], the NUL included.
pub const MAP_LEN: usize = 64;

/// A game event ([`ClientExt::game_event`]). `a`, `b`, `c` and `text` mean what the [`event`]
/// kind says; the rest are 0 / empty.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameEvent {
    pub kind: c_int,
    pub a: c_int,
    pub b: c_int,
    pub c: c_int,
    pub text: [c_char; NAME_LEN],
}

impl Default for GameEvent {
    fn default() -> Self {
        GameEvent { kind: 0, a: 0, b: 0, c: 0, text: [0; NAME_LEN] }
    }
}

/// [`GameEvent::kind`]s.
pub mod event {
    use core::ffi::c_int;
    /// The local player (re)spawned (`ResetHUD`).
    pub const SPAWN: c_int = 1;
    /// A death notice (`DeathMsg`): `a` the killer's slot (0 the world), `b` the victim's,
    /// `c` 1 when the victim is the local player; `text` the weapon as the kill feed names it.
    pub const DEATH: c_int = 2;
    /// The weapon in hand changed: `a` its TS id (0 kung fu), `text` its name.
    pub const WEAPON: c_int = 3;
    /// The local health or armour changed: `a` health, `b` armour.
    pub const HEALTH: c_int = 4;
    /// The local team changed: `a` the team number.
    pub const TEAM: c_int = 5;
    /// The round timer was set (`RoundTime`): `a` seconds, `b` its mode.
    pub const ROUND: c_int = 6;
    /// A map started (`InitHUD`): `text` the map name.
    pub const MAP: c_int = 7;
}

/// [`LocalState::flags`].
pub mod local {
    /// On the ground, as the last view calculation saw it.
    pub const ON_GROUND: u32 = 1;
    /// An observer (spectating).
    pub const OBSERVER: u32 = 2;
    /// In intermission.
    pub const INTERMISSION: u32 = 4;
    /// Minor 2: ducked (`FL_DUCKING` in the client's prediction: the duck has finished). A
    /// minor-1 client never sets it.
    pub const DUCKING: u32 = 8;
}

/// The local player ([`ClientExtApi::local_state`]), from the port's HUD state and view.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalState {
    /// Entity index (1..maxClients).
    pub index: c_int,
    pub alive: c_int,
    pub team: c_int,
    pub health: c_int,
    pub armor: c_int,
    /// The weapon in hand (TS id, 0 kung fu), its clip and its reserve, as the HUD shows them.
    pub weapon: c_int,
    pub clip: c_int,
    pub ammo: c_int,
    /// [`local`] bits.
    pub flags: u32,
    /// The eye position, view angles (pitch, yaw, roll), unit view vector and velocity.
    pub eye: [f32; 3],
    pub angles: [f32; 3],
    pub forward: [f32; 3],
    pub velocity: [f32; 3],
    pub speed: f32,
}

/// [`PlayerScore::flags`].
pub mod player {
    /// The local player.
    pub const LOCAL: u32 = 1;
    /// A spectator.
    pub const SPECTATOR: u32 = 2;
    /// On the local player's team (teamplay; always 0 without teams).
    pub const SAME_TEAM: u32 = 4;
}

/// A scoreboard row ([`ClientExtApi::player`], [`ClientExtApi::scores`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerScore {
    pub slot: c_int,
    pub team: c_int,
    pub frags: c_int,
    pub deaths: c_int,
    pub ping: c_int,
    /// [`player`] bits.
    pub flags: u32,
    /// The name's bytes as the engine holds them, cut to fit.
    pub name: [c_char; NAME_LEN],
}

impl Default for PlayerScore {
    fn default() -> Self {
        PlayerScore { slot: 0, team: 0, frags: 0, deaths: 0, ping: 0, flags: 0, name: [0; NAME_LEN] }
    }
}

/// [`AimInfo::hit`].
pub mod hit {
    use core::ffi::c_int;
    pub const NONE: c_int = 0;
    pub const WORLD: c_int = 1;
    pub const PLAYER: c_int = 2;
    pub const ENTITY: c_int = 3;
}

/// What the crosshair is on ([`ClientExtApi::aim`]): a trace from the eye along the view, 8192
/// units, with players solid (their studio boxes, as the HUD's crosshair ID traces).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AimInfo {
    /// A [`hit`] kind.
    pub hit: c_int,
    /// The entity hit (0 the world, -1 none), and the player slot when it is a player (else 0).
    pub entity: c_int,
    pub player: c_int,
    /// That player is on the local player's team (teamplay).
    pub same_team: c_int,
    pub distance: f32,
    pub point: [f32; 3],
}

/// A world trace ([`ClientExtApi::trace_line`]). Players are never solid in it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TraceInfo {
    pub fraction: f32,
    pub end: [f32; 3],
    /// The entity hit (0 the world), -1 for none.
    pub entity: c_int,
}

/// A player in view ([`ClientExtApi::visible_players`]): alive, not the local player, in this
/// frame's packet, inside the view, and reached by a world trace from the eye to its head or its
/// centre. "Alive" (from client minor 2): not since a `DeathMsg` named it the victim until its
/// entity moves as a living player again (docs/implementation/client-ext-api.md §6); a minor-1
/// client also lists a dying player.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VisiblePlayer {
    pub slot: c_int,
    pub team: c_int,
    pub same_team: c_int,
    pub distance: f32,
    /// The yaw from the view direction to the player (-180..180, positive left) and the pitch
    /// difference (positive up), degrees.
    pub bearing: f32,
    pub elevation: f32,
    /// Where it is on the screen (pixels) and in the world (its origin).
    pub screen: [f32; 2],
    pub origin: [f32; 3],
}

/// The view ([`ClientExtApi::view`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewInfo {
    pub width: c_int,
    pub height: c_int,
    /// The current field of view, degrees (zoom included).
    pub fov: f32,
    pub intermission: c_int,
    /// The map's name (`ts_dojo`), empty without a map.
    pub map: [c_char; MAP_LEN],
}

impl Default for ViewInfo {
    fn default() -> Self {
        ViewInfo { width: 0, height: 0, fov: 0.0, intermission: 0, map: [0; MAP_LEN] }
    }
}

/// [`ClientExtApi::renderer_kind`].
pub mod renderer {
    use core::ffi::c_int;
    pub const SOFTWARE: c_int = 0;
    pub const GL: c_int = 1;
    pub const GLES: c_int = 2;
}

/// The port's HUD elements ([`ClientExtApi::hud_set_hidden`], [`HudElement::id`]): ids, in the
/// webview spec's order (its §7.9 names, [`hud::NAMES`]).
pub mod hud {
    use core::ffi::CStr;
    /// The health box: the stance figure, health, the team bar.
    pub const HEALTH: u32 = 0;
    /// The kevlar icon and number.
    pub const ARMOUR: u32 = 1;
    /// The weapon in hand's name, clip and reserve.
    pub const AMMO: u32 = 2;
    /// The weapon list (slot selection).
    pub const WEAPON_SELECT: u32 = 3;
    /// The crosshair (or the bomb's wire panel), and the name of the player under it.
    pub const CROSSHAIR: u32 = 4;
    /// The kill feed (`DeathMsg`).
    pub const KILLFEED: u32 = 5;
    /// Chat (`SayText`).
    pub const CHAT: u32 = 6;
    /// TS's timed messages (`TSMessage`, the buy menu's notices).
    pub const NOTIFY: u32 = 7;
    /// Centre prints (`TextMsg` to the centre). The engine draws them: while hidden, new ones
    /// aren't printed, and no rectangle is known.
    pub const CENTRE: u32 = 8;
    /// HUD text (`HudText`, titles: the `CHudMessage` element).
    pub const HUDTEXT: u32 = 9;
    /// The round timer.
    pub const TIMER: u32 = 10;
    /// The free slots and credits boxes.
    pub const CASH: u32 = 11;
    /// The kung fu power label and bar, and the kung fu icon.
    pub const KUNGFU: u32 = 12;
    /// The stunt (movement) icon and the stunt cooldown steps.
    pub const STUNT: u32 = 13;
    /// The power-up name and time, and the slow-motion icon.
    pub const POWERUP: u32 = 14;
    /// The flag panel and the objective markers.
    pub const OBJECTIVE: u32 = 15;
    /// Pickup notices: TS 3.0 draws none, so hiding it changes nothing.
    pub const PICKUPS: u32 = 16;
    /// The status icons (`StatusIcon`).
    pub const STATUS_ICONS: u32 = 17;
    /// The spectator panel and the HLTV name tags.
    pub const SPECTATOR: u32 = 18;
    /// The scoreboard.
    pub const SCOREBOARD: u32 = 19;
    /// How many there are.
    pub const COUNT: u32 = 20;
    /// Each element's name, by id.
    pub const NAMES: [&CStr; COUNT as usize] = [
        c"health",
        c"armour",
        c"ammo",
        c"weapon_select",
        c"crosshair",
        c"killfeed",
        c"chat",
        c"notify",
        c"centre",
        c"hudtext",
        c"timer",
        c"cash",
        c"kungfu",
        c"stunt",
        c"powerup",
        c"objective",
        c"pickups",
        c"status_icons",
        c"spectator",
        c"scoreboard",
    ];
    /// Element `id`'s bit in a [`hud_set_hidden`](super::ClientExtApi::hud_set_hidden) mask.
    pub const fn bit(id: u32) -> u64 {
        1 << id
    }
    /// Every element.
    pub const ALL: u64 = (1 << COUNT) - 1;
}

/// [`HudElement::flags`].
pub mod hud_flag {
    /// Hidden (by any extension that isn't suspended).
    pub const HIDDEN: u32 = 1;
    /// It drew at the last HUD redraw (or would have, if hidden).
    pub const DRAWN: u32 = 2;
    /// [`HudElement::rect`](super::HudElement::rect) is known: where it last drew.
    pub const RECT: u32 = 4;
}

/// Bytes of [`HudElement::name`], the NUL included.
pub const HUD_NAME_LEN: usize = 16;

/// A HUD element ([`ClientExtApi::hud_elements`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HudElement {
    /// A [`hud`] id.
    pub id: u32,
    /// [`hud_flag`] bits.
    pub flags: u32,
    /// Where it last drew, screen pixels: x, y, width, height (with [`hud_flag::RECT`]; zero
    /// otherwise). The bounds of everything it drew, kept while it is hidden or draws nothing.
    pub rect: [c_int; 4],
    /// [`hud::NAMES`], NUL-terminated.
    pub name: [c_char; HUD_NAME_LEN],
}

impl Default for HudElement {
    fn default() -> Self {
        HudElement { id: 0, flags: 0, rect: [0; 4], name: [0; HUD_NAME_LEN] }
    }
}

/// The kinds of menu an extension can take over ([`ClientExtApi::menu_reply`]).
pub mod menu {
    use core::ffi::c_int;
    /// A text menu (`ShowMenu`).
    pub const TEXT: c_int = 1;
    /// A VGUI menu (`VGUIMenu`): the team menu (2), the map briefing (4), the MOTD window (5).
    pub const VGUI: c_int = 2;
    /// The message of the day (`MOTD`).
    pub const MOTD: c_int = 3;
}

/// [`ClientExt::menu_closed`] reasons.
pub mod menu_closed {
    use core::ffi::c_int;
    /// The server closed it (a `ShowMenu` without keys) or its display time ran out.
    pub const EXPIRED: c_int = 1;
    /// A newer menu of its kind came.
    pub const REPLACED: c_int = 2;
    /// `ResetHUD` (a spawn), or the connection ended.
    pub const RESET: c_int = 3;
    /// Its `menu_shown` gave it back: the port shows its own menu.
    pub const GIVEN_BACK: c_int = 4;
}

/// The functions the client offers. Every entry is non-NULL except [`Self::reserved`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ClientExtApi {
    /// `size_of::<ClientExtApi>()` in the client that built it.
    pub size: u32,
    /// [`VERSION`] and [`MINOR`] of the client that built it.
    pub version: u32,
    pub minor: u32,

    // ---- the host -----------------------------------------------------------------------------
    /// Prints `msg` to the console as `[tsmod_<name>] msg`.
    pub print: unsafe extern "C" fn(ext: ExtId, msg: *const c_char),
    /// Registers a console command for the process ([`CommandFn`]): [`OK`], [`EXISTS`] (taken by
    /// the port, the engine or an extension) or [`INVALID`] (a name with spaces, quotes or `;`,
    /// or over 63 bytes). The command does nothing while the extension is suspended.
    pub add_command: unsafe extern "C" fn(ext: ExtId, name: *const c_char, f: CommandFn) -> c_int,
    /// The running command's arguments (`argv[0]` its name); "" past the end.
    pub cmd_argc: extern "C" fn() -> c_int,
    pub cmd_argv: extern "C" fn(i: c_int) -> *const c_char,
    /// Registers a cvar (`FCVAR_*` flags): [`OK`], [`EXISTS`] if any cvar has the name (the port
    /// never re-registers one: FWGS would reset it) or [`INVALID`].
    pub register_cvar: unsafe extern "C" fn(ext: ExtId, name: *const c_char, value: *const c_char, flags: c_int) -> c_int,
    /// Any cvar's value and string (0 / "" for none); `set_cvar_value` is `Cvar_SetValue` (it
    /// sets the string too, at once): [`OK`] or [`NOT_FOUND`].
    pub cvar_value: unsafe extern "C" fn(name: *const c_char) -> f32,
    pub cvar_string: unsafe extern "C" fn(name: *const c_char, buf: *mut c_char, len: c_int) -> c_int,
    pub set_cvar_value: unsafe extern "C" fn(name: *const c_char, value: f32) -> c_int,
    /// Runs a console command line locally (`pfnClientCmd`), or sends one to the server
    /// (`pfnServerCmd`): [`OK`], or [`SUSPENDED`].
    pub client_cmd: unsafe extern "C" fn(ext: ExtId, cmd: *const c_char) -> c_int,
    pub server_cmd: unsafe extern "C" fn(ext: ExtId, cmd: *const c_char) -> c_int,
    /// The screen size, and the mouse: the cursor while a menu or an extension has it
    /// ([`ClientExt::wants_cursor`]), else the engine's mouse position. Pixels.
    pub screen_size: unsafe extern "C" fn(width: *mut c_int, height: *mut c_int),
    pub mouse_position: unsafe extern "C" fn(x: *mut c_int, y: *mut c_int),
    /// A GL or GLES function of the renderer's context, NULL for none or on the software renderer.
    /// Call it only from [`ClientExt::redraw`] or [`ClientExt::vid_init`].
    pub gl_proc: unsafe extern "C" fn(name: *const c_char) -> *mut c_void,
    /// A [`renderer`] kind.
    pub renderer_kind: extern "C" fn() -> c_int,
    /// The game directory (`ts`), the port's version (`3.0.8`), the server's address as in
    /// [`ClientExt::server_changed`] ("" when not connected).
    pub game_dir: unsafe extern "C" fn(buf: *mut c_char, len: c_int) -> c_int,
    pub port_version: unsafe extern "C" fn(buf: *mut c_char, len: c_int) -> c_int,
    pub server_address: unsafe extern "C" fn(buf: *mut c_char, len: c_int) -> c_int,
    /// The extension's own data directory, `<gamedir>/cl_plugins_data/<name>/`, with a trailing
    /// separator (created on first use): the only place it should write. [`INVALID`] for an id
    /// that isn't an extension's.
    pub data_dir: unsafe extern "C" fn(ext: ExtId, buf: *mut c_char, len: c_int) -> c_int,
    /// Starts a helper process: `path` (absolute, or relative to the game directory) with
    /// `argc` arguments, after the approval gate re-checked the file against the extension's
    /// approval (client-plugins.md §5.2). The process id (> 0), or [`REFUSED`] (not covered, or
    /// changed since it was approved), [`FAILED`], [`SUSPENDED`] or [`INVALID`]. The process
    /// inherits nothing but the environment and starts in the extension's data directory.
    pub verified_spawn: unsafe extern "C" fn(ext: ExtId, path: *const c_char, argc: c_int, argv: *const *const c_char) -> c_int,

    // ---- what the player sees -----------------------------------------------------------------
    /// The local player: [`OK`], or [`NOT_FOUND`] before there is one.
    pub local_state: unsafe extern "C" fn(out: *mut LocalState) -> c_int,
    /// One scoreboard row (`slot` 1..maxClients): [`OK`] or [`NOT_FOUND`] (no player there).
    pub player: unsafe extern "C" fn(slot: c_int, out: *mut PlayerScore) -> c_int,
    /// Every connected player's row, by slot, into `out` (room for `max`): the number of players
    /// (which may be more than `max`; only `max` are written).
    pub scores: unsafe extern "C" fn(out: *mut PlayerScore, max: c_int) -> c_int,
    /// The crosshair's target: [`OK`], [`NOT_FOUND`] without a local player, or [`UNAVAILABLE`].
    pub aim: unsafe extern "C" fn(out: *mut AimInfo) -> c_int,
    /// A world-only trace between two points (three floats each): [`OK`] or [`UNAVAILABLE`].
    pub trace_line: unsafe extern "C" fn(start: *const f32, end: *const f32, out: *mut TraceInfo) -> c_int,
    /// The players in view (see [`VisiblePlayer`]), nearest first, into `out` (room for `max`):
    /// how many there are. At most two traces per player, cached for the frame.
    pub visible_players: unsafe extern "C" fn(out: *mut VisiblePlayer, max: c_int) -> c_int,
    /// The view: [`OK`].
    pub view: unsafe extern "C" fn(out: *mut ViewInfo) -> c_int,
    /// A world point (three floats) on the screen, pixels (two floats): [`OK`], [`NOT_FOUND`]
    /// when it is behind the view, or [`UNAVAILABLE`]. Not bounds-checked.
    pub world_to_screen: unsafe extern "C" fn(point: *const f32, out: *mut f32) -> c_int,
    /// The name of a TS weapon id ("Glock-18", "Kung Fu"); "" for none.
    pub weapon_name: unsafe extern "C" fn(id: c_int, buf: *mut c_char, len: c_int) -> c_int,
    pub max_clients: extern "C" fn() -> c_int,

    // ---- minor 1 (NULL in a minor-0 client: check `minor` first) ------------------------------
    /// Claims a `TSExt` channel (1 to [`CHANNEL_MAX`] bytes of ASCII letters, digits, `_`, `-`,
    /// `.`; `webview`): its messages go to this extension's [`ClientExt::user_message`]. An
    /// extension may claim several. [`OK`] (also when it has it already), [`EXISTS`] (another
    /// extension has it) or [`INVALID`]. Messages on a channel nobody claimed are dropped.
    pub claim_channel: Option<unsafe extern "C" fn(ext: ExtId, name: *const c_char) -> c_int>,
    /// Ends a [`verdict::DEFER`]: `token` from the [`MessagePre`], `verdict` [`verdict::PASS`],
    /// [`verdict::BLOCK`] or [`verdict::REPLACE`] with `len` bytes at `data` (checked as a
    /// replacement from `message_pre` is). The run then goes on with the next extension. [`OK`],
    /// [`NOT_FOUND`] (no such hold of this extension's: released already, timed out, or the
    /// connection it came on ended) or [`INVALID`] (another verdict, a replacement over
    /// [`MESSAGE_MAX`]).
    pub release: Option<unsafe extern "C" fn(ext: ExtId, token: u32, verdict: c_int, data: *const u8, len: c_int) -> c_int>,

    // ---- minor 2 (NULL in an older client: check `minor` first) ---------------------------------
    /// Sets the [`hud`] elements this extension hides (`mask` of [`hud::bit`]s; 0 shows them
    /// all): [`OK`], [`SUSPENDED`], or [`INVALID`] (a bit past [`hud::COUNT`]; nothing changed).
    pub hud_set_hidden: Option<unsafe extern "C" fn(ext: ExtId, mask: u64) -> c_int>,
    /// Every [`hud`] element ([`HudElement`]), by id, into `out` (room for `max`): how many there
    /// are ([`hud::COUNT`]).
    pub hud_elements: Option<unsafe extern "C" fn(out: *mut HudElement, max: c_int) -> c_int>,
    /// Answers the menu of [`menu`] kind `kind` this extension took over, as the port's menu
    /// would: `choice` a key of a text menu (`"1"`..`"10"`, one the menu offers: `menuselect N`),
    /// a team of the team menu (`"1"`.. one it lists, or `"5"` auto-assign: `jointeam N`), or `""`
    /// (or NULL) to close it unanswered (Esc, cancel, the MOTD's OK). [`OK`] (the takeover ends),
    /// [`NOT_FOUND`] (no open takeover of that kind of this extension's, or its text isn't all
    /// here yet), [`INVALID`] (a choice the menu doesn't offer, or another kind) or [`SUSPENDED`].
    pub menu_reply: Option<unsafe extern "C" fn(ext: ExtId, kind: c_int, choice: *const c_char) -> c_int>,

    /// Functions a later [`MINOR`] defines; NULL here. Check [`Self::minor`] first.
    pub reserved: [Reserved; API_RESERVED],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout is the ABI: these sizes change only with [`VERSION`]. A minor version turns
    /// reserved slots into named ones and keeps them.
    #[test]
    fn sizes_are_pinned() {
        let p = size_of::<usize>();
        // Minor 0's sizes: 13 callbacks + 16 reserved, 29 functions + 32 reserved. Minor 1 named
        // 4 and 2 of the reserved slots, minor 2 another 2 and 3.
        assert_eq!(size_of::<ClientExt>(), 8 + 2 * p + (13 + 16) * p);
        assert_eq!(size_of::<ClientExt>(), 8 + 2 * p + (13 + 4 + 2 + EXT_RESERVED) * p);
        assert_eq!(size_of::<ClientExtApi>(), 12usize.next_multiple_of(p) + (29 + 32) * p);
        assert_eq!(size_of::<ClientExtApi>(), 12usize.next_multiple_of(p) + (29 + 2 + 3 + API_RESERVED) * p);
        assert_eq!(size_of::<HudElement>(), 8 + 16 + HUD_NAME_LEN);
        assert_eq!(size_of::<MessagePre>(), (2 * p + 12).next_multiple_of(p));
        assert_eq!(size_of::<CommandPre>(), 4 * p);
        assert_eq!(size_of::<UserCmd>(), 4 * 3 + 4 * 3 + 4 * 3 + 4 * 3);
        assert_eq!(size_of::<GameEvent>(), 16 + NAME_LEN);
        assert_eq!(size_of::<LocalState>(), 4 * 9 + 4 * 13);
        assert_eq!(size_of::<PlayerScore>(), 24 + NAME_LEN);
        assert_eq!(size_of::<ViewInfo>(), 16 + MAP_LEN);
    }

    #[test]
    fn suffixes() {
        assert_eq!(library_suffix(true, false), ".dll");
        assert_eq!(library_suffix(true, true), "_amd64.dll");
        assert_eq!(library_suffix(false, false), "_i386.so");
        assert_eq!(library_suffix(false, true), "_amd64.so");
        assert_eq!(ClientExt::EMPTY.size as usize, size_of::<ClientExt>());
    }

    /// Minor 1's named slots keep their offsets: minor 2's come after them, out of the reserved
    /// slots, so an extension built for minor 1 reads the same table.
    #[test]
    fn minor_slots_are_appended() {
        let p = size_of::<usize>();
        assert_eq!(core::mem::offset_of!(ClientExt, menu_shown), core::mem::offset_of!(ClientExt, server_cmd_pre) + p);
        assert_eq!(core::mem::offset_of!(ClientExt, reserved), core::mem::offset_of!(ClientExt, menu_closed) + p);
        assert_eq!(core::mem::offset_of!(ClientExtApi, hud_set_hidden), core::mem::offset_of!(ClientExtApi, release) + p);
        assert_eq!(core::mem::offset_of!(ClientExtApi, reserved), core::mem::offset_of!(ClientExtApi, menu_reply) + p);
    }

    #[test]
    fn hud_names_fit() {
        assert!(hud::NAMES.iter().all(|n| n.to_bytes_with_nul().len() <= HUD_NAME_LEN));
        assert_eq!(hud::ALL.count_ones(), hud::COUNT);
    }
}
