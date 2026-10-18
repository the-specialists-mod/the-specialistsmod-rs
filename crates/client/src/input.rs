//! Keyboard buttons, mouse look and `CL_CreateMove` (TS `InitInput` `10026b30`, `CL_CreateMove`
//! `10026570`, `CL_ButtonBits` `10026870`). Reference: decomp docs/client/architecture-hud.md §5.
//!
//! Mouse input has two back ends, picked at run time:
//! * **FWGS** (detected by its `host_ver` cvar): the engine reads the mouse and hands relative
//!   motion to our `IN_ClientLookEvent` export (an FWGS extension; when it is exported the engine
//!   no longer adds mouse look itself in `IN_EngineAppendMove`). Joystick/touch movement arrives in
//!   `IN_ClientMoveEvent`.
//! * **Steam GoldSrc** (pre- and post-anniversary): the client reads the mouse itself like the
//!   SDK's `inputw32.cpp`: on Windows `GetCursorPos`/`SetCursorPos` re-centering, or SDL2
//!   relative motion when `m_rawinput` is on; on Linux SDL2 relative motion. SDL2 entry points are
//!   looked up at run time in the already-loaded SDL2 library (no link-time dependency).
//!
//! Either way the deltas go through the same TS scaling (`m_filter`, TS acceleration, zoom or
//! `sensitivity`, `m_yaw`/`m_pitch`). While a menu shows the cursor, they move a virtual cursor
//! instead of the view (TS keeps such a cursor at `0x1018d17c`).

use crate::hud;
use goldsrc::Global;
use goldsrc::client::EngineCell;
use goldsrc::client::engine;
use goldsrc::sys::*;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_int, c_void};

/// SDK `kbutton_t`: the engine reads `state` through `KB_Find`.
#[repr(C)]
pub struct KButton {
    down: [c_int; 2],
    /// bit 0: held, bit 1: pressed since last frame, bit 2: released since last frame.
    state: c_int,
}

/// A `kbutton_t`. `KB_Find` hands the engine a pointer to it, so it lives in an [`EngineCell`].
pub struct Button(EngineCell<KButton>);

impl Button {
    pub(crate) const fn new() -> Self {
        Button(EngineCell::new(KButton { down: [0; 2], state: 0 }))
    }

    #[allow(clippy::mut_from_ref)]
    fn get(&self) -> &mut KButton {
        self.0.get()
    }

    /// The key number `KeyDown`/`KeyUp` (`0x100259a0`/`0x10025a10`) take from `Cmd_Argv(1)`:
    /// -1 when there is none, else MSVC `atoi` (`0x1006c042`), so text that isn't a number is key
    /// 0.
    fn key_arg() -> c_int {
        key_number(engine().cmd_argv(1).to_bytes())
    }

    /// SDK `KeyDown`.
    pub(crate) fn key_down(&self) {
        let b = self.get();
        let k = Self::key_arg();
        if k == b.down[0] || k == b.down[1] {
            return; // repeating key
        }
        if b.down[0] == 0 {
            b.down[0] = k;
        } else if b.down[1] == 0 {
            b.down[1] = k;
        } else {
            return; // three keys on one button
        }
        if b.state & 1 == 0 {
            b.state |= 1 + 2;
        }
    }

    /// SDK `KeyUp`.
    pub(crate) fn key_up(&self) {
        let b = self.get();
        let k = Self::key_arg();
        if k == -1 {
            // Typed manually at the console: release everything.
            b.down = [0; 2];
            b.state = 4;
            return;
        }
        if b.down[0] == k {
            b.down[0] = 0;
        } else if b.down[1] == k {
            b.down[1] = 0;
        } else {
            return;
        }
        if b.down != [0; 2] || b.state & 1 == 0 {
            return;
        }
        b.state = (b.state & !1) | 4;
    }

    /// Down at any point since the last `CL_ButtonBits` reset. Only for the buttons
    /// [`button_bits`] reports, since it is what clears the pressed edge (bit 1).
    pub(crate) fn held(&self) -> bool {
        self.get().state & 3 != 0
    }

    /// Down right now (`state & 1`): how TS tests the buttons `CL_ButtonBits` doesn't report
    /// (`in_speed`, `in_strafe`, `in_klook`, `in_jlook`). Nothing clears their pressed edge, so
    /// [`held`](Self::held) would stay true after the first release.
    pub(crate) fn down(&self) -> bool {
        self.get().state & 1 != 0
    }

    /// SDK `CL_KeyState`: fraction of the frame the key was down (0, 0.25, 0.5, 0.75 or 1).
    pub(crate) fn key_state(&self) -> f32 {
        let b = self.get();
        let (impulse_down, impulse_up, down) = (b.state & 2 != 0, b.state & 4 != 0, b.state & 1 != 0);
        let val = match (impulse_down, impulse_up, down) {
            (false, false, d) => d as i32 as f32,
            (true, false, true) => 0.5,
            (true, false, false) => 0.0,
            (false, true, false) => 0.0,
            (false, true, true) => 0.0,
            (true, true, false) => 0.25,
            (true, true, true) => 0.75,
        };
        b.state &= 1;
        val
    }

    /// Release the button as if its keys went up (SDK `CL_ResetButtonBits`).
    fn release(&self) {
        let b = self.get();
        b.state &= !1;
    }

    fn ptr(&self) -> *mut c_void {
        self.0.as_ptr().cast()
    }
}

macro_rules! buttons {
    ($($ident:ident;)*) => {
        $(pub static $ident: Button = Button::new();)*
    };
}

buttons! {
    IN_KLOOK;
    IN_JLOOK;
    IN_MLOOK;
    IN_GRAPH;
    IN_BREAK;
    IN_STRAFE;
    IN_LEFTB;
    IN_RIGHTB;
    IN_FWD;
    IN_BK;
    IN_LOOKUP;
    IN_LOOKDOWN;
    IN_MOVELEFTB;
    IN_MOVERIGHTB;
    IN_UPK;
    IN_DOWNK;
    IN_SPEED;
    IN_JMP;
    IN_DCK;
    IN_ATK;
    IN_ATK2;
    IN_USEK;
    IN_RELOD;
    IN_ALT1B;
    IN_ALT2B;
    IN_SCOREB;
}

/// The `kbutton_t` list `KB_Find` walks (`g_kbkeys`, `0x100df5c8`): a name and its button.
type KbEntry = (&'static CStr, &'static Button);

/// `g_kbkeys` (`0x100df5c8`), newest first as `KB_Add` (`0x100258c0`) links them.
static KB_KEYS: Global<RefCell<Vec<KbEntry>>> = Global(RefCell::new(Vec::new()));

/// `KB_Find` (`0x10025880`): the first entry whose name matches, case-insensitively (TS compares
/// with `_stricmp`, `0x10076d90`).
fn kb_find_entry(list: &[KbEntry], name: &CStr) -> Option<&'static Button> {
    list.iter().find(|(n, _)| n.to_bytes().eq_ignore_ascii_case(name.to_bytes())).map(|(_, b)| *b)
}

/// `KB_Add` (`0x100258c0`): links a new entry at the head unless the name is already listed.
fn kb_add(list: &mut Vec<KbEntry>, name: &'static CStr, button: &'static Button) {
    if kb_find_entry(list, name).is_none() {
        list.insert(0, (name, button));
    }
}

/// `KB_Init` (`0x10025930`, the end of `InitInput`): only `in_graph`, `in_mlook` and `in_jlook`
/// are listed, so `KB_Find` answers null for every other name.
fn kb_init(list: &mut Vec<KbEntry>) {
    list.clear();
    kb_add(list, c"in_graph", &IN_GRAPH);
    kb_add(list, c"in_mlook", &IN_MLOOK);
    kb_add(list, c"in_jlook", &IN_JLOOK);
}

/// `KB_Find` export (`0x10025880`): the engine looks buttons up by name. Null once
/// `KB_Shutdown` has emptied the list.
pub fn find(name: &CStr) -> *mut c_void {
    kb_find_entry(&KB_KEYS.0.borrow(), name).map_or(std::ptr::null_mut(), Button::ptr)
}

/// `ShutdownInput` (`0x10027240`, the body of the `HUD_Shutdown` export `0x10027250`):
/// `IN_Shutdown`, which on Windows is just `IN_DeactivateMouse` (`0x100272a0`), then
/// `KB_Shutdown` (`0x10025970`), which frees the `kbutton_t` list.
pub fn shutdown() {
    deactivate_mouse();
    KB_KEYS.0.borrow_mut().clear();
}

// ---------------------------------------------------------------------------------------------
// Anti-script gate (§5.3): `HUD_Key_Event` sets it when a physical key's binding is one of the
// gated commands; `HUD_Frame` clears it. Gated `+` handlers do nothing without it.

static REAL_KEY: Global<Cell<bool>> = Global(Cell::new(false));

/// The bindings that set the gate, exactly as TS compares them.
pub const GATED_BINDINGS: [&str; 9] = ["+attack", "+attack2", "+alt2", "+reload", "+forward", "+back", "+moveleft", "+moveright", "+jump"];

/// `ts_dev_scriptinput 1` (port-only development aid): with `sv_cheats 1`, scripted `+attack`,
/// `+jump`, … work without a physical key, for automated in-engine tests.
static DEV_SCRIPT_INPUT: Global<Cell<Cvar>> = Global(Cell::new(Cvar::NULL));

/// Whether a gated `+` command may press its button: a physical key this frame, the dev aid, or a
/// ruleset without the gate ([`ClientRules::input_gate`](crate::gameplay::ClientRules::input_gate)).
pub fn gate_open() -> bool {
    REAL_KEY.0.get() || !crate::gameplay::current().input_gate() || dev_input_allowed()
}

/// `ts_dev_scriptinput 1` with `sv_cheats 1`: scripted input may stand in for a player's (the
/// gate above, and the dev aids `ts_dev_key` and `ts_dev_extsuspend`, `crate::ext`).
pub fn dev_input_allowed() -> bool {
    DEV_SCRIPT_INPUT.0.get().value() != 0.0 && cvar_ptr_value(c"sv_cheats") != 0.0
}

/// A menu or a client extension (`crate::ext::cursor_wanted`) owns the mouse: the view holds
/// still and mouse motion moves the virtual cursor. Clicks go to the menu only when a menu owns
/// it (`mouse_event`); an extension gets them as keys through `HUD_Key_Event`.
pub fn cursor_owned() -> bool {
    crate::plugins::wants_cursor() || crate::menus::wants_cursor() || crate::ext::cursor_wanted()
}

/// TS `FUN_10023450`: gated `+forward`/`+back`/`+moveleft`/`+moveright`/`+attack`/`+attack2`/
/// `+jump` presses and `+duck` share one 0.2 s timer. When the timer has run out it restarts, and
/// a press with bit 0 set (`+attack` only) sends `respawn`. The server ignores it unless the
/// player is an observer whose respawn wait is over, so this is how a click spawns you in.
static NEXT_KEY_TAP: Global<Cell<f32>> = Global(Cell::new(0.0));

/// TS keeps the timer in `CHudSpectator` (`+0xc94`), which `Init` and `VidInit` zero. The client
/// clock restarts with each map, so a timer left from the previous map would block every click
/// (and so spawning) until the new map's clock caught up with the old one.
pub fn reset_key_tap() {
    NEXT_KEY_TAP.0.set(0.0);
}

/// A `+` command's key tap: `bits` are TS 3.0's (`FUN_10023450`), `buttons` the command's `IN_*`
/// bit for a ruleset's own key hook
/// ([`ClientRules::hltv_buttons`](crate::gameplay::ClientRules::hltv_buttons)).
fn key_tap(bits: u8, buttons: u32) {
    if crate::gameplay::current().hltv_buttons(&mut crate::spectator::EngineHltv, buttons as i32, true) {
        return;
    }
    let now = engine().client_time();
    if key_tap_step(&NEXT_KEY_TAP.0, now, bits) == Some(true) && crate::gameplay::current().attack_sends_respawn() {
        engine().server_cmd("respawn");
    }
}

/// A movement command's release, for a ruleset's own key hook (TS 3.0's `-` commands only
/// release the button).
fn key_release(buttons: u32) {
    crate::gameplay::current().hltv_buttons(&mut crate::spectator::EngineHltv, buttons as i32, false);
}

/// Pure part of `key_tap`: `None` while the timer runs, else whether to send `respawn`.
fn key_tap_step(next: &Cell<f32>, now: f32, bits: u8) -> Option<bool> {
    if next.get() > now {
        return None;
    }
    next.set(now + 0.2);
    Some(bits & 1 != 0)
}

fn cvar_ptr_value(name: &CStr) -> f32 {
    Cvar::find(name).value()
}

/// Pure part of `HUD_Key_Event`'s gate check (used by tests).
pub fn binding_is_gated(binding: &str) -> bool {
    GATED_BINDINGS.contains(&binding)
}

/// `KB_ConvertString` (`0x10025740`, SDK input.cpp; TS's only caller is `ShowMenu` at
/// `0x10028843`): each `+command` in `text` becomes `[key]`, the name of the key bound to it
/// (`Key_LookupBinding(command)`, the name without its `+`), or stays as written when nothing is
/// bound. The command is the `+` and the alphanumerics after it (`isalnum`, `0x1006c45a`), at
/// most 63 bytes; a longer run goes on as plain text. A lone `+` isn't looked up. The text ends
/// at the first NUL, as TS's C string does.
///
/// Out-of-bounds guards: TS's `isalnum` indexes `_pctype` with the sign-extended byte, so a
/// byte >= 0x80 reads memory before the table; here it is not alphanumeric (as the C locale's
/// table says for the in-bounds bytes). TS writes into a 4096-byte stack buffer with no bound;
/// here output past 4095 bytes is dropped.
pub fn kb_convert_string(text: &[u8], lookup: &dyn Fn(&[u8]) -> Option<Vec<u8>>) -> Vec<u8> {
    const SZ: usize = 4096;
    let at = |i: usize| text.get(i).copied().unwrap_or(0);
    let mut out = Vec::with_capacity(text.len());
    let mut put = |b: u8| {
        if out.len() < SZ - 1 {
            out.push(b);
        }
    };
    let mut p = 0;
    while at(p) != 0 {
        if at(p) != b'+' {
            put(at(p));
            p += 1;
            continue;
        }
        // 0x1002578b: copy while (isalnum(*p) || pEnd == binding) && pEnd - binding < 63.
        let mut binding = Vec::new();
        while (at(p).is_ascii_alphanumeric() || binding.is_empty()) && binding.len() < 63 {
            binding.push(at(p));
            p += 1;
            if at(p) == 0 {
                break;
            }
        }
        // 0x100257c7: `strlen(binding + 1)`; only a non-empty name is looked up.
        let key = if binding.len() > 1 { lookup(&binding[1..]) } else { None };
        match key {
            Some(k) => {
                put(b'[');
                k.iter().take_while(|&&c| c != 0).for_each(|&c| put(c));
                put(b']');
            }
            None => binding.iter().for_each(|&c| put(c)),
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Cvars

/// Cvar handle (`cvar_t*` owned by the engine).
pub use goldsrc::client::Cvar;

macro_rules! cvars {
    ($($field:ident = $name:literal, $default:literal, $flags:expr;)*) => {
        #[derive(Clone, Copy)]
        pub struct InputCvars { $(pub $field: Cvar,)* }
        impl InputCvars {
            const fn null() -> Self { InputCvars { $($field: Cvar::NULL,)* } }
            fn register() -> Self {
                InputCvars { $($field: Cvar::register($name, $default, $flags),)* }
            }
        }
    };
}

const ARCHIVE: i32 = 1;

// `InitInput` 10026b30 and `IN_Init` 10028320 registrations (architecture-hud.md §2.1), in order.
cvars! {
    lookstrafe = c"lookstrafe", c"0", ARCHIVE;
    lookspring = c"lookspring", c"0", ARCHIVE;
    cl_anglespeedkey = c"cl_anglespeedkey", c"0.67", 0;
    cl_yawspeed = c"cl_yawspeed", c"210", 0;
    cl_pitchspeed = c"cl_pitchspeed", c"225", 0;
    cl_upspeed = c"cl_upspeed", c"320", 0;
    cl_forwardspeed = c"cl_forwardspeed", c"400", ARCHIVE;
    cl_backspeed = c"cl_backspeed", c"400", ARCHIVE;
    cl_sidespeed = c"cl_sidespeed", c"400", 0;
    cl_movespeedkey = c"cl_movespeedkey", c"0.3", 0;
    cl_vsmoothing = c"cl_vsmoothing", c"0.05", ARCHIVE;
    m_pitch = c"m_pitch", c"0.022", ARCHIVE;
    m_yaw = c"m_yaw", c"0.022", ARCHIVE;
    m_forward = c"m_forward", c"1", ARCHIVE;
    m_side = c"m_side", c"0.8", ARCHIVE;
    cl_fx = c"cl_fx", c"1", ARCHIVE;
    cam_back = c"cam_back", c"90", ARCHIVE;
    cam_right = c"cam_right", c"0", ARCHIVE;
    cam_up = c"cam_up", c"12", ARCHIVE;
    cl_camtype = c"cl_camtype", c"0", ARCHIVE;
    hud_r = c"hud_r", c"150", ARCHIVE;
    hud_g = c"hud_g", c"175", ARCHIVE;
    hud_b = c"hud_b", c"255", ARCHIVE;
    hud_color = c"hud_color", c"150,175,255", ARCHIVE;
    cl_lasercolor = c"cl_lasercolor", c"10", ARCHIVE;
    cl_buykevlar = c"cl_buykevlar", c"1", ARCHIVE;
    nohud = c"nohud", c"0", 0;
    cl_realhud = c"cl_realhud", c"0", ARCHIVE;
    cl_shadow = c"cl_shadow", c"2", ARCHIVE;
    cl_wavedetail = c"cl_wavedetail", c"0", ARCHIVE;
    cl_thirdpersonswitch = c"cl_thirdpersonswitch", c"0", ARCHIVE;
    cl_usepowerup = c"cl_usepowerup", c"1", ARCHIVE;
    cl_laser = c"cl_laser", c"0", ARCHIVE;
    cl_solidcross = c"cl_solidcross", c"2", ARCHIVE;
    cl_thirdperson = c"cl_thirdperson", c"0", ARCHIVE;
    cl_attackswitch = c"cl_attackswitch", c"0", ARCHIVE;
    cl_thirdtransparency = c"cl_thirdtransparency", c"1", ARCHIVE;
    cl_motionblur = c"cl_motionblur", c"1", ARCHIVE;
    cl_gore = c"cl_gore", c"2", ARCHIVE;
    cl_showequipment = c"cl_showequipment", c"1", ARCHIVE;
    m_filter = c"m_filter", c"0", ARCHIVE;
    sensitivity = c"sensitivity", c"3", ARCHIVE;
    m_accel = c"m_accel", c"0", ARCHIVE;
    m_accelexp = c"m_accelexp", c"1.2", ARCHIVE;
    m_accelmax = c"m_accelmax", c"0", ARCHIVE;
    joystick = c"joystick", c"0", ARCHIVE;
    joyname = c"joyname", c"joystick", 0;
    joyadvanced = c"joyadvanced", c"0", 0;
    joyadvaxisx = c"joyadvaxisx", c"0", 0;
    joyadvaxisy = c"joyadvaxisy", c"0", 0;
    joyadvaxisz = c"joyadvaxisz", c"0", 0;
    joyadvaxisr = c"joyadvaxisr", c"0", 0;
    joyadvaxisu = c"joyadvaxisu", c"0", 0;
    joyadvaxisv = c"joyadvaxisv", c"0", 0;
    joyforwardthreshold = c"joyforwardthreshold", c"0.15", 0;
    joysidethreshold = c"joysidethreshold", c"0.15", 0;
    joypitchthreshold = c"joypitchthreshold", c"0.15", 0;
    joyyawthreshold = c"joyyawthreshold", c"0.15", 0;
    joyforwardsensitivity = c"joyforwardsensitivity", c"-1.0", 0;
    joysidesensitivity = c"joysidesensitivity", c"-1.0", 0;
    joypitchsensitivity = c"joypitchsensitivity", c"1.0", 0;
    joyyawsensitivity = c"joyyawsensitivity", c"-1.0", 0;
    joywwhack1 = c"joywwhack1", c"0.0", 0;
    joywwhack2 = c"joywwhack2", c"0.0", 0;
}

static CVARS: Global<Cell<InputCvars>> = Global(Cell::new(InputCvars::null()));

pub fn cvars() -> InputCvars {
    CVARS.0.get()
}

/// HUD colour from `hud_r/g/b`, used by every TS draw.
pub fn hud_rgb() -> [i32; 3] {
    let c = cvars();
    [c.hud_r.value() as i32, c.hud_g.value() as i32, c.hud_b.value() as i32]
}

// ---------------------------------------------------------------------------------------------
// Commands

/// `cl_attackswitch` (§5.1): swap `+attack2` and `+alt2` when set and a weapon other than kung fu,
/// grenades, knives or the katana is held (TS weapon kinds 2, 3, 10, 11 keep the direct mapping).
/// A ruleset without the swap ([`ClientRules::attack_switch`](crate::gameplay::ClientRules::attack_switch))
/// keeps the direct mapping whatever the cvar says.
pub fn attack_switched() -> bool {
    if cvars().cl_attackswitch.value() == 0.0 || !crate::gameplay::current().attack_switch() {
        return false;
    }
    let id = crate::state::with(|s| s.weapon);
    if id <= 0 {
        return false;
    }
    !matches!(weapon_kind(id), 2 | 3 | 10 | 11)
}

/// TS weapon kind byte (`info+0x348`, server weapons.md §2) from the shared table.
pub fn weapon_kind(id: i32) -> i32 {
    use shared::weapons::WeaponKind as K;
    let Some(w) = shared::weapons::tables().weapon(id as u8) else { return 11 };
    match w.kind {
        K::Kungfu => 0,
        K::Shotgun => 1,
        K::Grenade => 2,
        K::Knife => 3,
        K::Akimbo => 4,
        K::AkimboOnly => 5,
        K::Rifle => 6,
        K::Sniper => 7,
        K::Pistol => 8,
        K::Smg => 9,
        K::Katana => 10,
    }
}

macro_rules! cmd {
    ($name:literal, $body:expr) => {{
        extern "C" fn f() {
            $body
        }
        engine().add_command($name, f);
    }};
}

fn register_commands() {
    // Order follows InitInput (architecture-hud.md §2.2).
    cmd!(c"+moveup", IN_UPK.key_down());
    cmd!(c"-moveup", IN_UPK.key_up());
    cmd!(c"+movedown", IN_DOWNK.key_down());
    cmd!(c"-movedown", IN_DOWNK.key_up());
    cmd!(c"+left", IN_LEFTB.key_down());
    cmd!(c"-left", IN_LEFTB.key_up());
    cmd!(c"+right", IN_RIGHTB.key_down());
    cmd!(c"-right", IN_RIGHTB.key_up());
    // Gated movement.
    cmd!(c"+forward", {
        if gate_open() {
            IN_FWD.key_down();
            key_tap(0x08, IN_FORWARD);
        }
    });
    cmd!(c"-forward", {
        IN_FWD.key_up();
        key_release(IN_FORWARD);
    });
    cmd!(c"+back", {
        if gate_open() {
            IN_BK.key_down();
            key_tap(0x10, IN_BACK);
        }
    });
    cmd!(c"-back", {
        IN_BK.key_up();
        key_release(IN_BACK);
    });
    cmd!(c"+moveleft", {
        if gate_open() {
            IN_MOVELEFTB.key_down();
            key_tap(0, IN_MOVELEFT);
        }
    });
    cmd!(c"-moveleft", {
        IN_MOVELEFTB.key_up();
        key_release(IN_MOVELEFT);
    });
    cmd!(c"+moveright", {
        if gate_open() {
            IN_MOVERIGHTB.key_down();
            key_tap(0, IN_MOVERIGHT);
        }
    });
    cmd!(c"-moveright", {
        IN_MOVERIGHTB.key_up();
        key_release(IN_MOVERIGHT);
    });
    cmd!(c"+lookup", IN_LOOKUP.key_down());
    cmd!(c"-lookup", IN_LOOKUP.key_up());
    cmd!(c"+lookdown", IN_LOOKDOWN.key_down());
    cmd!(c"-lookdown", IN_LOOKDOWN.key_up());
    cmd!(c"+speed", IN_SPEED.key_down());
    cmd!(c"-speed", IN_SPEED.key_up());
    cmd!(c"+attack", {
        if gate_open() {
            IN_ATK.key_down();
            key_tap(0x01, IN_ATTACK);
        }
    });
    cmd!(c"-attack", IN_ATK.key_up());
    // +attack2 / +alt2 drive one of two kbuttons depending on cl_attackswitch. TS's +attack2 also
    // runs `special` (`_special` to the server through the viewport).
    cmd!(c"+attack2", {
        if gate_open() {
            if attack_switched() {
                IN_ALT2B.key_down()
            } else {
                IN_ATK2.key_down()
            }
            engine().server_cmd("_special");
            key_tap(0, IN_ATTACK2);
        }
    });
    // The `-` handlers (`0x10025fc0`, `0x10026180`) release only the kbutton the current
    // `cl_attackswitch` state picks, so a weapon switch while the key is held leaves the other
    // one down until the next press and release under the same mapping, as in TS.
    cmd!(c"-attack2", if attack_switched() { IN_ALT2B.key_up() } else { IN_ATK2.key_up() });
    cmd!(c"+use", IN_USEK.key_down());
    cmd!(c"-use", IN_USEK.key_up());
    cmd!(c"+jump", {
        if gate_open() {
            IN_JMP.key_down();
            key_tap(0x02, IN_JUMP);
        }
    });
    cmd!(c"-jump", IN_JMP.key_up());
    cmd!(c"impulse", {
        let n: i32 = engine().cmd_argv(1).to_str().ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        IMPULSE.0.set(n);
    });
    cmd!(c"+klook", IN_KLOOK.key_down());
    cmd!(c"-klook", IN_KLOOK.key_up());
    cmd!(c"+jlook", IN_JLOOK.key_down());
    cmd!(c"-jlook", IN_JLOOK.key_up());
    cmd!(c"+duck", {
        IN_DCK.key_down();
        key_tap(0x04, IN_DUCK);
    });
    cmd!(c"-duck", IN_DCK.key_up());
    cmd!(
        c"+reload",
        if gate_open() {
            IN_RELOD.key_down()
        }
    );
    cmd!(c"-reload", IN_RELOD.key_up());
    cmd!(c"+alt1", IN_ALT1B.key_down());
    cmd!(c"-alt1", IN_ALT1B.key_up());
    cmd!(c"+alt2", {
        if gate_open() {
            if attack_switched() { IN_ATK2.key_down() } else { IN_ALT2B.key_down() }
        }
    });
    cmd!(c"-alt2", if attack_switched() { IN_ATK2.key_up() } else { IN_ALT2B.key_up() });
    cmd!(c"+score", {
        IN_SCOREB.key_down();
        crate::menus::show_scores(true);
    });
    cmd!(c"-score", {
        IN_SCOREB.key_up();
        crate::menus::show_scores(false);
    });
    cmd!(c"+showscores", {
        IN_SCOREB.key_down();
        crate::menus::show_scores(true);
    });
    cmd!(c"-showscores", {
        IN_SCOREB.key_up();
        crate::menus::show_scores(false);
    });
    cmd!(c"+graph", IN_GRAPH.key_down());
    cmd!(c"-graph", IN_GRAPH.key_up());
    cmd!(c"+break", IN_BREAK.key_down());
    cmd!(c"-break", IN_BREAK.key_up());
}

static IMPULSE: Global<Cell<i32>> = Global(Cell::new(0));

// ---------------------------------------------------------------------------------------------
// Mouse

#[derive(Default)]
struct Mouse {
    is_fwgs: bool,
    initialized: bool,
    active: bool,
    old_buttons: i32,
    accum: [f32; 2],
    old: [f32; 2],
    /// Joystick/touch movement from FWGS `IN_ClientMoveEvent`.
    move_accum: [f32; 2],
    /// Virtual cursor (TS `0x1018d17c/180`), used while a menu shows the cursor.
    cursor: [f32; 2],
    #[cfg(windows)]
    win: goldsrc::platform::win::State,
}

static MOUSE: Global<RefCell<Option<Mouse>>> = Global(RefCell::new(None));

/// The closure must not call back into this module (it would hit the `RefCell` borrow).
fn mouse<R>(f: impl FnOnce(&mut Mouse) -> R) -> R {
    let mut m = MOUSE.0.borrow_mut();
    f(m.get_or_insert_with(Mouse::default))
}

/// The virtual cursor position, clamped to the screen.
pub fn cursor() -> (i32, i32) {
    mouse(|m| (m.cursor[0] as i32, m.cursor[1] as i32))
}

/// Centre the virtual cursor (TS does this when the buy menu opens).
pub fn center_cursor() {
    let s = crate::hud::screen();
    mouse(|m| m.cursor = [s.width as f32 / 2.0, s.height as f32 / 2.0]);
}

/// Whether the engine is Xash3D FWGS (it has the `host_ver` cvar; Steam GoldSrc doesn't).
pub fn is_fwgs() -> bool {
    mouse(|m| m.is_fwgs)
}

/// TS mouse acceleration (`IN_MouseMove` `0x100276b9..0x100277c8`, `pow` at `0x1006c490`):
/// `d += sign · min(|raw|^m_accelexp · m_accel, m_accelmax)`. The power is of the raw (unfiltered)
/// count and the sign is +1 only for a raw count above 0, while `d` is the filtered value; a cap
/// of 0 means none, and the cap applies once the sum reaches it. Unrounded until the store.
pub fn accelerate(d: f32, raw: f32, accel: f32, exp: f32, max: f32) -> f32 {
    let mut add = (raw.abs() as f64).powf(exp as f64) * accel as f64;
    if max != 0.0 && max as f64 <= add {
        add = max as f64;
    }
    let sign = if raw > 0.0 { 1.0 } else { -1.0 };
    (sign * add + d as f64) as f32
}

/// `m_filter` (`0x10027586..0x10027614`): the mean of this count and the previous *output*
/// (TS keeps the filtered value as the old one, unlike the SDK's raw one), a filtered x within
/// ±0.01 becoming 0. Returns the new (and next old) values.
fn filter_mouse(raw: [f32; 2], old: [f32; 2], on: bool) -> [f32; 2] {
    if !on {
        return raw;
    }
    let x = raw[0] as f64 + old[0] as f64;
    let x = if x * 0.5 < 0.01 && x * 0.5 > -0.01 { 0.0 } else { (x * 0.5) as f32 };
    [x, ((raw[1] as f64 + old[1] as f64) * 0.5) as f32]
}

/// Scale raw mouse counts to view-angle deltas and apply them (or move the virtual cursor).
/// `IN_MouseMove` (`0x100274d0`): filter, then the zoom sensitivity alone or the acceleration
/// and `sensitivity` (unclamped), then `yaw -= x·m_yaw`, `pitch += y·m_pitch` clamped to ±89.
fn apply_mouse(raw: [f32; 2], view: &mut [f32; 3]) {
    let c = cvars();
    if cursor_owned() {
        let s = crate::hud::screen();
        mouse(|m| {
            m.cursor[0] = (m.cursor[0] + raw[0]).clamp(0.0, cursor_max(s.width));
            m.cursor[1] = (m.cursor[1] + raw[1]).clamp(0.0, cursor_max(s.height));
        });
        return;
    }
    let [mut mx, mut my] = mouse(|m| {
        m.old = filter_mouse(raw, m.old, c.m_filter.value() != 0.0);
        m.old
    });
    let zoom = crate::hud::zoom_sensitivity();
    if zoom != 0.0 {
        mx = (zoom as f64 * mx as f64) as f32;
        my = (zoom as f64 * my as f64) as f32;
    } else {
        let (a, e, x) = (c.m_accel.value(), c.m_accelexp.value(), c.m_accelmax.value());
        mx = accelerate(mx, raw[0], a, e, x);
        my = accelerate(my, raw[1], a, e, x);
        let sens = c.sensitivity.value();
        mx *= sens;
        my *= sens;
    }
    view[YAW] = (view[YAW] as f64 - mx as f64 * c.m_yaw.value() as f64) as f32;
    view[PITCH] = ((my as f64 * c.m_pitch.value() as f64 + view[PITCH] as f64) as f32).clamp(-89.0, 89.0);
}

/// The last cursor pixel on a screen `size` wide (or high). Before `VidInit` fills the screen
/// info its size is 0, which would make `clamp`'s upper bound negative (a panic), so it is 0.
fn cursor_max(size: i32) -> f32 {
    size.saturating_sub(1).max(0) as f32
}

const PITCH: usize = 0;
const YAW: usize = 1;
const ROLL: usize = 2;
const K_MOUSE1: i32 = 241;

/// Raw mouse counts since the last call, for the active back end.
fn take_mouse_delta() -> [f32; 2] {
    let (fwgs, active) = mouse(|m| (m.is_fwgs, m.active));
    if fwgs {
        // FWGS: already accumulated by IN_ClientLookEvent (in degrees: yaw = -x*m_yaw, pitch =
        // y*m_pitch). Convert back to counts so the TS scaling applies uniformly.
        let c = cvars();
        let a = mouse(|m| std::mem::take(&mut m.accum));
        let (my, mp) = (c.m_yaw.value(), c.m_pitch.value());
        return [if my != 0.0 { -a[0] / my } else { 0.0 }, if mp != 0.0 { a[1] / mp } else { 0.0 }];
    }
    if !active {
        return [0.0; 2];
    }
    let mut d = mouse(|m| std::mem::take(&mut m.accum));
    let now = native::delta();
    d[0] += now[0];
    d[1] += now[1];
    d
}

/// The mouse motion since the last read, in raw counts (cursor pixels with the Windows cursor),
/// for the camera's mouse modes. TS's `CAM_Think` reads the Win32 cursor's offset from the window
/// centre and re-centres it; with mouse look stood down (`camera::mouse_in_use`), the motion
/// pending here is that offset on every back end. The camera also drains it where TS only
/// re-centres, which throws the motion away before mouse look sees it.
pub(crate) fn take_cursor_offset() -> [f32; 2] {
    take_mouse_delta()
}

/// FWGS `IN_ClientLookEvent` (exported by the dll crate): relative mouse look (degrees, already
/// scaled by m_yaw/m_pitch).
pub fn look_event(relyaw: f32, relpitch: f32) {
    mouse(|m| {
        m.accum[0] += relyaw;
        m.accum[1] += relpitch;
    });
}

/// FWGS `IN_ClientMoveEvent` (exported by the dll crate): joystick/touch movement (-1..1).
pub fn move_event(forwardmove: f32, sidemove: f32) {
    mouse(|m| {
        m.move_accum[0] += forwardmove;
        m.move_accum[1] += sidemove;
    });
}

pub fn activate_mouse() {
    mouse(|m| {
        if m.initialized && !m.is_fwgs {
            native::activate(m);
            m.active = true;
        } else if m.initialized {
            m.active = true;
        }
    });
}

pub fn deactivate_mouse() {
    mouse(|m| {
        if m.initialized {
            if !m.is_fwgs {
                native::deactivate(m);
            }
            m.active = false;
        }
    });
}

/// SDK `IN_MouseEvent`: forward button edges to the engine as K_MOUSE1.. keys, except that
/// clicks go to the open menu while it shows the cursor (TS routes them to `CTSBuyMenu`).
pub fn mouse_event(mstate: i32) {
    // Port addition: an offer card or the plugins panel (client-plugins.md §7.3, §8) takes the
    // clicks first, whatever else holds the mouse; clicks reach them only from here, never from a
    // key or a bind.
    if crate::plugins::wants_cursor() {
        let old = mouse(|m| std::mem::replace(&mut m.old_buttons, mstate));
        let (x, y) = cursor();
        crate::plugins::mouse(mstate, old, x, y);
        return;
    }
    // TS's `IN_MouseEvent` (100273d0) ignores the buttons while the camera holds the mouse.
    if crate::camera::mouse_in_use() {
        return;
    }
    let old = mouse(|m| std::mem::replace(&mut m.old_buttons, mstate));
    // A press on the scoreboard enters voice squelch mode instead of firing.
    if mstate & !old != 0 && crate::menus::scoreboard_press() {
        return;
    }
    if crate::menus::wants_cursor() {
        let (x, y) = cursor();
        crate::menus::mouse_buttons(mstate, old, x, y);
        return;
    }
    for i in 0..5 {
        let (now, was) = (mstate & (1 << i) != 0, old & (1 << i) != 0);
        if now != was {
            engine().key_event(K_MOUSE1 + i, now);
        }
    }
}

pub fn clear_states() {
    mouse(|m| {
        if m.active {
            m.accum = [0.0; 2];
            m.old_buttons = 0;
        }
    });
}

/// SDK `IN_Accumulate`: collect motion between frames (Steam GoldSrc only).
pub fn accumulate() {
    let (fwgs, active) = mouse(|m| (m.is_fwgs, m.active));
    // TS's `IN_Accumulate` (100278b0) also stands down while the camera holds the mouse.
    if fwgs || !active || crate::camera::mouse_in_use() {
        return;
    }
    let d = native::delta();
    mouse(|m| {
        m.accum[0] += d[0];
        m.accum[1] += d[1];
    });
}

/// Platform mouse readers for Steam GoldSrc.
mod native {
    use super::Mouse;

    #[cfg(windows)]
    pub fn activate(m: &mut Mouse) {
        goldsrc::platform::win::activate(&mut m.win);
    }
    #[cfg(windows)]
    pub fn deactivate(m: &mut Mouse) {
        goldsrc::platform::win::deactivate(&mut m.win);
    }
    #[cfg(windows)]
    pub fn delta() -> [f32; 2] {
        goldsrc::platform::win::delta()
    }

    #[cfg(not(windows))]
    pub fn activate(_m: &mut Mouse) {
        goldsrc::platform::sdl::set_relative(true);
    }
    #[cfg(not(windows))]
    pub fn deactivate(_m: &mut Mouse) {
        goldsrc::platform::sdl::set_relative(false);
    }
    #[cfg(not(windows))]
    pub fn delta() -> [f32; 2] {
        goldsrc::platform::sdl::relative_delta()
    }
}

// ---------------------------------------------------------------------------------------------
// Frame / key entry points

pub fn init() {
    CVARS.0.set(InputCvars::register());
    DEV_SCRIPT_INPUT.0.set(Cvar::register(c"ts_dev_scriptinput", c"0", 0));
    register_commands();
    // `InitInput` (10026b30) ends with CAM_Init, IN_Init and V_Init.
    crate::view::cam_init();
    let fwgs = !Cvar::find(c"host_ver").is_null();
    mouse(|m| {
        m.is_fwgs = fwgs;
        // SDK IN_StartupMouse: `-nomouse` disables the mouse entirely.
        m.initialized = true;
        #[cfg(windows)]
        goldsrc::platform::win::startup(&mut m.win);
    });
    crate::joystick::init(fwgs);
    // `KB_Init` (`0x10025930`) runs between `IN_Init` and `V_Init`.
    kb_init(&mut KB_KEYS.0.borrow_mut());
    // TS removed +mlook: mouse look is always on. Report it to the engine through in_mlook.
    IN_MLOOK.get().state = 1;
    engine().print(if fwgs { "[ts] input: FWGS mouse (IN_ClientLookEvent)\n" } else { "[ts] input: client-side mouse (GoldSrc)\n" });
}

/// `IN_Commands` (10027ca0), from `HUD_UpdateClientData`: joystick buttons (Steam GoldSrc only).
pub fn commands() {
    if !is_fwgs() {
        crate::joystick::commands();
    }
}

/// The end of `HUD_Frame` (`0x1001ab27`): TS clears the anti-script gate (`0x100df5cc`) after the
/// frame's commands ran.
pub fn frame() {
    REAL_KEY.0.set(false);
}

/// `HUD_Key_Event` (10025a80): the slot-11 key and the anti-script gate ([`key_event`]), then
/// `TeamFortressViewport::KeyInput` ([`crate::menus::key_event`]). 0 when a menu took the key;
/// 1 hands it to the engine, which for Escape opens the main menu (newer FWGS) or runs its
/// `cancelselect` binding (Steam GoldSrc, FWGS 0.21), which sends `escape`.
pub fn hud_key_event(down: bool, key: i32, binding: &CStr) -> i32 {
    key_event(down, key, binding);
    let ret = if crate::menus::key_event(down, key, binding) { 0 } else { 1 };
    if key == K_ESCAPE {
        crate::events::dev_key_log(|| {
            format!("HUD_Key_Event escape down={} binding=\"{}\" -> {ret}", down as i32, binding.to_string_lossy())
        });
    }
    ret
}

/// GoldSrc `keydefs.h` `K_ESCAPE`.
const K_ESCAPE: i32 = 27;

/// `HUD_Key_Event`'s first part: TS forwards keydown 'a' to the weapon element as slot 11, then
/// opens the gate when the binding is one of the gated commands. `eventcode` guards only the
/// slot-11 shortcut (0x10025a85): the gate value is `eax = 1` from 0x10025aa5, which the binding
/// compares never touch, so a gated key's release opens the gate too (issue #144).
pub fn key_event(down: bool, key: i32, binding: &CStr) {
    if down && key == b'a' as i32 {
        hud::select_slot(11);
    }
    if binding_is_gated(binding.to_str().unwrap_or("")) {
        REAL_KEY.0.set(true);
    }
}

/// `CL_AdjustAngles`' turn-speed clamp (`0x10026330..0x100263c1`): `cl_pitchspeed` ≤ 220 and
/// `cl_yawspeed` ≤ 200, and anything not above 0 (NaN included) is 0.
fn turn_speed(v: f32, max: f32) -> f32 {
    if v > max || v > 0.0 { v.min(max) } else { 0.0 }
}

/// SDK `anglemod` (`0x10029b80`): the angle truncated to 1/65536 of a turn, in [0, 360).
/// `__ftol` truncates toward zero, so a negative angle wraps through the mask.
pub fn anglemod(a: f32) -> f32 {
    let i = (a as f64 * (65536.0 / 360.0)) as i64 as i32;
    ((i & 0xffff) as f64 * (360.0 / 65536.0)) as f32
}

/// TS `CL_AdjustAngles` (`0x10026330`): keyboard turning with the anti-turn-bind clamps. The
/// yaw goes through `anglemod` every call unless `in_strafe` is held, which quantises the mouse
/// yaw of the previous frame too. Products on the x87 at 53 bits (`f64`), rounded at each store.
fn adjust_angles(frametime: f32, view: &mut [f32; 3]) {
    let c = cvars();
    let speed = if IN_SPEED.down() {
        let k = c.cl_anglespeedkey.value();
        (frametime as f64 * if k <= 0.67 { 0.67f32 } else { k } as f64) as f32
    } else {
        frametime
    };
    let yawspeed = turn_speed(c.cl_yawspeed.value(), 200.0) as f64;
    let pitchspeed = turn_speed(c.cl_pitchspeed.value(), 220.0) as f64;
    let step = |key: f32, rate: f64| key as f64 * speed as f64 * rate;
    if !IN_STRAFE.down() {
        view[YAW] = (view[YAW] as f64 - step(IN_RIGHTB.key_state(), yawspeed)) as f32;
        view[YAW] = (step(IN_LEFTB.key_state(), yawspeed) + view[YAW] as f64) as f32;
        view[YAW] = anglemod(view[YAW]);
    }
    if IN_KLOOK.down() {
        crate::view::stop_pitch_drift();
        view[PITCH] = (view[PITCH] as f64 - step(IN_FWD.key_state(), pitchspeed)) as f32;
        view[PITCH] = (step(IN_BK.key_state(), pitchspeed) + view[PITCH] as f64) as f32;
    }
    let up = IN_LOOKUP.key_state();
    let down = IN_LOOKDOWN.key_state();
    view[PITCH] = (step(down, pitchspeed) + (view[PITCH] as f64 - step(up, pitchspeed))) as f32;
    if up != 0.0 || down != 0.0 {
        crate::view::stop_pitch_drift();
    }
    view[PITCH] = view[PITCH].clamp(-89.0, 89.0);
    view[ROLL] = view[ROLL].clamp(-50.0, 50.0);
}

/// [`Button::key_arg`] on the argument's bytes.
fn key_number(arg: &[u8]) -> c_int {
    if arg.is_empty() { -1 } else { crate::hud::statusbar::atoi(arg) }
}

/// The command's view angles while `g_iAlive` (`0x100df578`), sent instead while it isn't.
static LIVE_ANGLES: Global<Cell<[f32; 3]>> = Global(Cell::new([0.0; 3]));
/// View angles held while a menu owns the mouse.
static MENU_ANGLES: Global<Cell<Option<[f32; 3]>>> = Global(Cell::new(None));
/// The angles `create_move` last gave the engine, to tell a server `svc_setangle` apart.
static LAST_SET_ANGLES: Global<Cell<Option<[f32; 3]>>> = Global(Cell::new(None));

/// `CL_ButtonBits`: kbuttons to `usercmd_t::buttons`, with TS's bit map (§5.1). Clears the
/// pressed/released edges like the SDK.
pub fn button_bits(reset: bool) -> u32 {
    let mut bits = 0u32;
    let map: [(&Button, u32); 15] = [
        (&IN_ATK, IN_ATTACK),
        (&IN_DCK, IN_DUCK),
        (&IN_JMP, IN_JUMP),
        (&IN_FWD, IN_FORWARD),
        (&IN_BK, IN_BACK),
        (&IN_USEK, IN_USE),
        (&IN_ALT2B, shared::ts::IN_ALT2 as u32),
        (&IN_LEFTB, IN_LEFT),
        (&IN_RIGHTB, IN_RIGHT),
        (&IN_MOVELEFTB, IN_MOVELEFT),
        (&IN_MOVERIGHTB, IN_MOVERIGHT),
        (&IN_ATK2, IN_ATTACK2),
        (&IN_RELOD, IN_RELOAD),
        (&IN_ALT1B, IN_ALT1),
        (&IN_SCOREB, IN_SCORE),
    ];
    for (b, bit) in map {
        if b.held() {
            bits |= bit;
        }
        if reset {
            b.get().state &= !2;
        }
    }
    // +score is also forced while dead or in intermission (TS `0x100d5b20`).
    let (health, intermission) = (crate::state::with(|s| s.health), hud::intermission());
    if health < 1 || intermission {
        bits |= IN_SCORE;
    }
    bits
}

pub fn create_move(frametime: f32, cmd: &mut usercmd_t, active: bool) {
    let e = engine();
    let c = cvars();
    let mut view = e.view_angles();

    // A menu that owns the mouse keeps the view still and moves the virtual cursor instead.
    if cursor_owned() {
        let held = menu_hold(MENU_ANGLES.0.get(), LAST_SET_ANGLES.0.get(), view);
        MENU_ANGLES.0.set(Some(held));
        view = held;
    } else {
        MENU_ANGLES.0.set(None);
    }

    *cmd = usercmd_t { ..Default::default() };
    if active {
        adjust_angles(frametime, &mut view);
        let strafe = IN_STRAFE.down();
        if strafe {
            cmd.sidemove += c.cl_sidespeed.value() * IN_RIGHTB.key_state();
            cmd.sidemove -= c.cl_sidespeed.value() * IN_LEFTB.key_state();
        }
        cmd.sidemove += c.cl_sidespeed.value() * IN_MOVERIGHTB.key_state();
        cmd.sidemove -= c.cl_sidespeed.value() * IN_MOVELEFTB.key_state();
        cmd.upmove += c.cl_upspeed.value() * IN_UPK.key_state();
        cmd.upmove -= c.cl_upspeed.value() * IN_DOWNK.key_state();
        if !IN_KLOOK.down() {
            cmd.forwardmove += c.cl_forwardspeed.value() * IN_FWD.key_state();
            cmd.forwardmove -= c.cl_backspeed.value() * IN_BK.key_state();
        }
        if IN_SPEED.down() {
            let s = c.cl_movespeedkey.value();
            cmd.forwardmove *= s;
            cmd.sidemove *= s;
            cmd.upmove *= s;
        }
        clamp_to_maxspeed(cmd, e.client_maxspeed());
        // Joystick/touch movement from FWGS (TS's `IN_Move` joystick comes after the clamp too).
        let mv = mouse(|m| std::mem::take(&mut m.move_accum));
        cmd.forwardmove += mv[0] * c.cl_forwardspeed.value();
        cmd.sidemove += mv[1] * c.cl_sidespeed.value();

        // IN_Move (100282e0): mouse look, then the joystick (Steam GoldSrc; FWGS sent it above).
        // TS's `IN_MouseMove` (100274d0) stops the pitch drift first, every frame, so the view never
        // drifts while the mouse is active. IN_Move skips the mouse entirely while
        // `+cammousemove`/`+camdistance` hold it (`iMouseInUse` 0x100df394): the motion is left
        // for `CAM_Think` to move the third-person camera with.
        if !crate::camera::mouse_in_use() {
            if mouse(|m| m.active) {
                crate::view::stop_pitch_drift();
            }
            let raw = take_mouse_delta();
            if !hud::intermission() {
                apply_mouse(raw, &mut view);
            }
        }
        if !is_fwgs() {
            crate::joystick::joy_move(frametime, &mut view, &mut cmd.forwardmove, &mut cmd.sidemove);
        }
    } else if !crate::camera::mouse_in_use() {
        let _ = take_mouse_delta();
    }

    cmd.impulse = IMPULSE.0.replace(0) as u8;
    let mut bits = button_bits(true);
    // The weapon menu takes +attack / +attack2 as select / drop (TS CHudTSWeapons::Think).
    let consumed = hud::weapon_menu_buttons(bits);
    if consumed & IN_ATTACK != 0 {
        IN_ATK.release();
    }
    if consumed & IN_ATTACK2 != 0 {
        IN_ATK2.release();
    }
    bits &= !consumed;
    // The scoreboard's squelch mode takes the mouse: no +attack (`CL_CreateMove` 10026570).
    if crate::voice::in_squelch_mode() {
        bits &= !IN_ATTACK;
    }
    bits |= joystick_move_bits(c.joystick.value(), cmd.forwardmove);
    cmd.buttons = bits as u16;

    if cursor_owned()
        && let Some(a) = MENU_ANGLES.0.get()
    {
        view = a;
    }
    e.set_view_angles(view);
    LAST_SET_ANGLES.0.set(Some(view));
    cmd.viewangles = command_angles(view);
}

/// `g_iAlive` (`0x100936a4`, initially 1): `HUD_TxferPredictionData` (`0x1001bb9a`) sets it to
/// `iuser1 != 0 || deadflag == 0`. TS's server never writes `deadflag`, so against it this stays 1.
static ALIVE: Global<Cell<bool>> = Global(Cell::new(true));

/// `HUD_TxferPredictionData`'s `g_iAlive` update, from the server's client data.
pub fn set_alive(iuser1: i32, deadflag: i32) {
    ALIVE.0.set(iuser1 != 0 || deadflag == 0);
}

/// The end of `CL_CreateMove` (`0x100267e0..0x10026840`): the command carries the engine's view
/// angles, saved while `g_iAlive` (`0x100df578`); otherwise the saved ones. Only the command's
/// angles freeze: the engine's view angles keep following the mouse.
fn command_angles(view: [f32; 3]) -> [f32; 3] {
    if ALIVE.0.get() {
        LIVE_ANGLES.0.set(view);
        view
    } else {
        LIVE_ANGLES.0.get()
    }
}

/// `CL_CreateMove` (`0x100266f2..0x1002674f`, SDK): a movement vector longer than the client's
/// `maxspeed` (when that isn't 0) is scaled down to it. The length is taken unrounded on the
/// x87 and the factor `maxspeed / length` is kept unrounded for the three products.
fn clamp_to_maxspeed(cmd: &mut usercmd_t, maxspeed: f32) {
    if maxspeed == 0.0 {
        return;
    }
    let (f, s, u) = (cmd.forwardmove as f64, cmd.sidemove as f64, cmd.upmove as f64);
    let len = (f * f + s * s + u * u).sqrt();
    // `fcoms maxspeed; test ah, 0x41`: scaled only when the length is above it (not NaN).
    if len > maxspeed as f64 {
        let k = maxspeed as f64 / len;
        cmd.forwardmove = (k * f) as f32;
        cmd.sidemove = (k * s) as f32;
        cmd.upmove = (k * u) as f32;
    }
}

/// `CL_CreateMove` (`0x100267a0..0x100267dc`): with the `joystick` cvar set, a forward move adds
/// `IN_FORWARD` and a backward one `IN_BACK`, whatever moved it.
fn joystick_move_bits(joystick: f32, forwardmove: f32) -> u32 {
    if joystick == 0.0 {
        0
    } else if forwardmove > 0.0 {
        IN_FORWARD
    } else if forwardmove < 0.0 {
        IN_BACK
    } else {
        0
    }
}

/// The view angles to hold while a menu owns the mouse. The hold is against the player's own
/// turning (keys, joystick); angles the engine set since the last `CL_CreateMove` came from the
/// server (`svc_setangle`: spawn, teleport, `ts_face`) and replace the held ones. TS has no hold:
/// its VGUI menus only stop the mouse, so the server's angles always apply.
fn menu_hold(held: Option<[f32; 3]>, last_set: Option<[f32; 3]>, engine: [f32; 3]) -> [f32; 3] {
    match held {
        Some(h) if last_set == Some(engine) => h,
        _ => engine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `KB_ConvertString` (`0x10025740`).
    #[test]
    fn kb_convert_string_swaps_bound_commands() {
        let looked_up = std::cell::RefCell::new(Vec::new());
        let bind = |b: &[u8]| {
            looked_up.borrow_mut().push(b.to_vec());
            match b {
                b"attack" => Some(b"MOUSE1".to_vec()),
                b"use" => Some(b"e".to_vec()),
                _ => None,
            }
        };
        let conv = |t: &[u8]| kb_convert_string(t, &bind);
        // The name is looked up without its `+` and ends at the first non-alphanumeric byte.
        assert_eq!(conv(b"1. +attack!\n2. +use+use"), b"1. [MOUSE1]!\n2. [e][e]");
        assert_eq!(*looked_up.borrow(), [b"attack".to_vec(), b"use".to_vec(), b"use".to_vec()]);
        looked_up.borrow_mut().clear();
        // Unbound names stay; a lone `+` (or `++`) isn't looked up; `_` and `-` end a name.
        assert_eq!(conv(b"+jump + ++ +a_b +x-y"), b"+jump + ++ +a_b +x-y");
        assert_eq!(*looked_up.borrow(), [b"jump".to_vec(), b"a".to_vec(), b"x".to_vec()]);
        looked_up.borrow_mut().clear();
        // Bytes >= 0x80 are not alphanumeric (the guarded `_pctype` read); text ends at a NUL.
        assert_eq!(conv(b"+use\xe9 a\0+use"), b"[e]\xe9 a");
        // At most 63 bytes (with the `+`) make the name; the rest is plain text.
        let long = [b"+".as_slice(), &[b'a'; 70]].concat();
        looked_up.borrow_mut().clear();
        assert_eq!(conv(&long), long);
        assert_eq!(*looked_up.borrow(), [vec![b'a'; 62]]);
        // The 4096-byte output buffer: output past 4095 bytes is dropped (TS overflows it).
        assert_eq!(conv(&b"+use".repeat(2000)).len(), 4095);
    }

    /// `KB_Init`/`KB_Find`/`KB_Shutdown` (`0x10025930`/`0x10025880`/`0x10025970`): only the three
    /// buttons TS lists are found, case-insensitively, and none after the shutdown.
    #[test]
    fn kb_list_holds_only_ts_buttons_until_shutdown() {
        let mut list = Vec::new();
        kb_init(&mut list);
        assert_eq!(list.iter().map(|(n, _)| *n).collect::<Vec<_>>(), [c"in_jlook", c"in_mlook", c"in_graph"], "newest first");
        assert!(std::ptr::eq(kb_find_entry(&list, c"in_mlook").unwrap(), &IN_MLOOK));
        assert!(std::ptr::eq(kb_find_entry(&list, c"IN_Graph").unwrap(), &IN_GRAPH), "_stricmp");
        assert!(std::ptr::eq(kb_find_entry(&list, c"in_jlook").unwrap(), &IN_JLOOK));
        assert!(kb_find_entry(&list, c"in_attack").is_none(), "TS never lists the other buttons");
        kb_add(&mut list, c"in_mlook", &IN_KLOOK);
        assert_eq!(list.len(), 3, "KB_Add skips a listed name");
        kb_init(&mut list);
        assert_eq!(list.len(), 3, "KB_Init restarts the list");
        KB_KEYS.0.borrow_mut().clone_from(&list);
        assert!(!find(c"in_graph").is_null());
        KB_KEYS.0.borrow_mut().clear(); // `shutdown`'s `KB_Shutdown` half
        assert!(find(c"in_graph").is_null());
    }

    #[test]
    fn key_tap_sends_respawn_on_attack_only_and_throttles() {
        let next = Cell::new(0.0);
        assert_eq!(key_tap_step(&next, 1.0, 0x01), Some(true)); // +attack: respawn
        assert_eq!(key_tap_step(&next, 1.1, 0x01), None); // within 0.2 s
        assert_eq!(key_tap_step(&next, 1.25, 0x08), Some(false)); // +forward restarts the timer
        assert_eq!(key_tap_step(&next, 1.35, 0x01), None); // ...which also holds back +attack
        assert_eq!(key_tap_step(&next, 1.5, 0x01), Some(true));
    }

    /// TS registers each client cvar once (`goldsrc::client::Cvar`'s registration rule); a second
    /// `pfnRegisterVariable` for a name resets it on FWGS and is refused on GoldSrc. Scans the
    /// crate's sources for `Cvar::register(c"…"` and the `cvars!` table above.
    #[test]
    fn each_cvar_is_registered_once() {
        fn names_after(text: &str, marker: &str, out: &mut Vec<String>) {
            for (i, _) in text.match_indices(marker) {
                let rest = &text[i + marker.len()..];
                out.push(rest[..rest.find('"').unwrap()].to_string());
            }
        }
        fn scan(dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    scan(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    names_after(&text, concat!("Cvar::register", "(c\""), out);
                    if path.ends_with("input.rs") {
                        let table = &text[text.find(concat!("cvars", "! {")).unwrap()..];
                        names_after(&table[..table.find("\n}").unwrap()], concat!("= ", "c\""), out);
                    }
                }
            }
        }
        let mut names = Vec::new();
        scan(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut names);
        assert!(names.len() > 80, "scan found only {} registrations", names.len());
        names.sort();
        let dups: Vec<_> = names.windows(2).filter(|w| w[0] == w[1]).map(|w| w[0].clone()).collect();
        assert!(dups.is_empty(), "cvars registered more than once: {dups:?}");
        // TS's CAM_Init, CHud::Init fog, V_Init and Scheme_Init cvars (#65).
        for name in [
            "cam_command",
            "cam_snapto",
            "cam_idealyaw",
            "cam_idealpitch",
            "cam_idealdist",
            "cam_contain",
            "c_maxpitch",
            "c_minpitch",
            "c_maxyaw",
            "c_minyaw",
            "c_maxdistance",
            "c_mindistance",
            "gl_fog",
            "gl_fog_start",
            "gl_fog_end",
            "gl_fog_red",
            "gl_fog_green",
            "gl_fog_blue",
            "scr_ofsx",
            "scr_ofsy",
            "scr_ofsz",
            "bitmapfonts",
        ] {
            assert!(names.binary_search(&name.to_string()).is_ok(), "{name} is not registered");
        }
    }

    #[test]
    fn key_tap_timer_reset_on_map_change() {
        // Last click at 127 s on the old map; the new map's clock starts lower.
        assert_eq!(key_tap_step(&NEXT_KEY_TAP.0, 127.0, 0x01), Some(true));
        assert_eq!(key_tap_step(&NEXT_KEY_TAP.0, 73.0, 0x01), None, "the old map's timer blocks the click");
        reset_key_tap(); // spectator::vid_init
        assert_eq!(key_tap_step(&NEXT_KEY_TAP.0, 73.0, 0x01), Some(true));
    }

    #[test]
    fn menu_hold_takes_server_angles() {
        let (a, b) = ([0.0, 10.0, 0.0], [5.0, 90.0, 0.0]);
        assert_eq!(menu_hold(None, Some(a), a), a, "a menu that just opened holds the current view");
        assert_eq!(menu_hold(Some(a), Some(a), a), a, "nothing changed: keep holding");
        assert_eq!(menu_hold(Some(a), Some(a), b), b, "svc_setangle while the menu is open wins");
        assert_eq!(menu_hold(Some(a), None, b), b);
    }

    #[test]
    fn gated_bindings_match_ts() {
        assert!(binding_is_gated("+attack"));
        assert!(binding_is_gated("+jump"));
        assert!(!binding_is_gated("+duck"));
        assert!(!binding_is_gated("+alt1"));
        assert!(!binding_is_gated("+attack;wait")); // literal comparison, like TS
        // HUD_Key_Event opens the gate on a gated key's release as well as its press.
        for down in [true, false] {
            REAL_KEY.0.set(false);
            key_event(down, b'w' as i32, c"+forward");
            assert!(REAL_KEY.0.get(), "down={down}");
            REAL_KEY.0.set(false);
            key_event(down, b'c' as i32, c"+duck");
            assert!(!REAL_KEY.0.get(), "down={down}");
        }
    }

    #[test]
    fn ts_acceleration() {
        assert_eq!(accelerate(5.0, 5.0, 0.0, 1.2, 0.0), 5.0);
        let a = accelerate(4.0, 4.0, 0.5, 1.0, 0.0);
        assert!((a - 6.0).abs() < 1e-5);
        let a = accelerate(-4.0, -4.0, 0.5, 1.0, 1.0);
        assert!((a + 5.0).abs() < 1e-5, "capped by m_accelmax");
        // The power is of the raw count, the sign is the raw count's (0 counts as negative),
        // and it is added to the filtered value.
        let a = accelerate(3.0, 0.0, 0.5, 1.0, 0.0);
        assert_eq!(a, 3.0);
        let a = accelerate(1.0, 4.0, 0.5, 1.0, 0.0);
        assert!((a - 3.0).abs() < 1e-5);
        // A negative m_accel decelerates.
        let a = accelerate(4.0, 4.0, -0.5, 1.0, 0.0);
        assert!((a - 2.0).abs() < 1e-5);
    }

    /// `m_filter` keeps the filtered value as the next old one and zeroes a tiny x.
    #[test]
    fn mouse_filter_is_recursive() {
        assert_eq!(filter_mouse([4.0, 6.0], [9.0, 9.0], false), [4.0, 6.0]);
        let a = filter_mouse([4.0, 6.0], [0.0, 0.0], true);
        assert_eq!(a, [2.0, 3.0]);
        let b = filter_mouse([0.0, 0.0], a, true);
        assert_eq!(b, [1.0, 1.5], "the old value is the previous output, not the raw count");
        assert_eq!(filter_mouse([0.01, 0.01], [0.0, 0.0], true), [0.0, 0.005]);
    }

    /// `anglemod` truncates to 1/65536 of a turn and wraps negatives.
    #[test]
    fn anglemod_quantises() {
        assert_eq!(anglemod(90.0), 90.0);
        assert_eq!(anglemod(360.0), 0.0);
        assert_eq!(anglemod(-90.0), 270.0);
        let a = anglemod(10.003);
        assert!(a < 10.003 && 10.003 - a < 360.0 / 65536.0, "{a}");
        assert_eq!(anglemod(a), a);
    }

    #[test]
    fn turn_speed_clamps() {
        assert_eq!(turn_speed(500.0, 220.0), 220.0);
        assert_eq!(turn_speed(100.0, 220.0), 100.0);
        assert_eq!(turn_speed(-100.0, 220.0), 0.0);
        assert_eq!(turn_speed(f32::NAN, 200.0), 0.0);
    }

    /// `CL_CreateMove`'s maxspeed clamp and the joystick's forward/back bits.
    #[test]
    fn create_move_clamp_and_joystick_bits() {
        let mut cmd = usercmd_t { forwardmove: 400.0, sidemove: 400.0, ..Default::default() };
        clamp_to_maxspeed(&mut cmd, 300.0);
        let len = (cmd.forwardmove * cmd.forwardmove + cmd.sidemove * cmd.sidemove).sqrt();
        assert!((len - 300.0).abs() < 1e-3, "{len}");
        let mut cmd = usercmd_t { forwardmove: 400.0, ..Default::default() };
        clamp_to_maxspeed(&mut cmd, 0.0);
        assert_eq!(cmd.forwardmove, 400.0, "maxspeed 0: no clamp");
        clamp_to_maxspeed(&mut cmd, 400.0);
        assert_eq!(cmd.forwardmove, 400.0, "at the limit: no clamp");
        assert_eq!(joystick_move_bits(0.0, 100.0), 0);
        assert_eq!(joystick_move_bits(1.0, 100.0), IN_FORWARD);
        assert_eq!(joystick_move_bits(1.0, -1.0), IN_BACK);
        assert_eq!(joystick_move_bits(1.0, 0.0), 0);
    }

    /// `KeyDown`/`KeyUp`'s key number: none is -1, anything else is `atoi`.
    #[test]
    fn key_number_is_atoi() {
        assert_eq!((key_number(b""), key_number(b"241"), key_number(b"foo"), key_number(b"12x")), (-1, 241, 0, 12));
    }

    /// `g_iAlive`: only the command's angles freeze, at the last ones sent alive.
    #[test]
    fn dead_freezes_command_angles_only() {
        set_alive(0, 0);
        assert_eq!(command_angles([1.0, 2.0, 0.0]), [1.0, 2.0, 0.0]);
        set_alive(0, 2);
        assert_eq!(command_angles([5.0, 6.0, 0.0]), [1.0, 2.0, 0.0]);
        set_alive(4, 2);
        assert_eq!(command_angles([5.0, 6.0, 0.0]), [5.0, 6.0, 0.0], "an observer isn't frozen");
        set_alive(0, 0);
    }

    /// The virtual cursor's bound before `VidInit` (screen 0×0) and for nonsense sizes: never
    /// negative, so `clamp` can't panic.
    #[test]
    fn cursor_bound_never_negative() {
        assert_eq!((cursor_max(640), cursor_max(1), cursor_max(0), cursor_max(-5), cursor_max(i32::MIN)), (639.0, 0.0, 0.0, 0.0, 0.0));
        for size in [0, i32::MIN, i32::MAX] {
            for v in [f32::NAN, -1e30, 1e30, 5.0] {
                let _ = v.clamp(0.0, cursor_max(size));
            }
        }
    }

    /// `KB_ConvertString` on random menu text (a `ShowMenu` payload), with a binding lookup that
    /// returns long names.
    #[test]
    fn kb_convert_string_garbage() {
        let mut x = 0x5eed_1234_u32;
        let lookup = |b: &[u8]| b.len().is_multiple_of(2).then(|| vec![b'k'; b.len() * 100]);
        for n in 0..500 {
            let text: Vec<u8> = (0..n * 7 % 3000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    if x & 1 == 0 { b'+' } else { (x >> 8) as u8 }
                })
                .collect();
            assert!(kb_convert_string(&text, &lookup).len() < 4096);
        }
        assert_eq!(kb_convert_string(b"+", &lookup), b"+");
        assert_eq!(kb_convert_string(b"", &lookup), b"");
    }

    #[test]
    fn key_state_fractions() {
        let b = Button::new();
        b.get().state = 1 | 2;
        assert_eq!(b.key_state(), 0.5);
        assert_eq!(b.get().state, 1);
        assert_eq!(b.key_state(), 1.0);
        b.get().state = 4;
        assert_eq!(b.key_state(), 0.0);
    }

    /// A press and release of `+speed` leaves `state` 6 (pressed and released edges, not down):
    /// TS tests `state & 1`, so walking ends with the release.
    #[test]
    fn released_speed_key_is_not_down() {
        let b = Button::new();
        b.get().state = 2 | 4;
        assert!(!b.down());
        b.get().state = 1 | 2;
        assert!(b.down());
    }

    #[test]
    fn button_bits_layout() {
        assert_eq!(shared::ts::IN_ALT2 as u32, IN_CANCEL);
        assert_eq!(IN_ALT1, 0x4000);
        assert_eq!(IN_SCORE, 0x8000);
    }
}
