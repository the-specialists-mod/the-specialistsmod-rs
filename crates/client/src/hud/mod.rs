//! HUD: message hooks, HUD state, and one immediate-mode drawing pass in TS order. Owner: HUD/UI
//! work package. Reference: decomp docs/client/architecture-hud.md §3-4, §7.
//!
//! TS's `CHud` element list (§4.1) becomes plain functions called from [`redraw`] in the same
//! order: the SDK health/battery/ammo elements TS kept only for bookkeeping draw nothing, then
//! `CHudMessage` (HudText), death notices, text messages, `CHudTSWeapons`, `CHudTSStatus`, say
//! text and `CHudMenu`.

pub mod draw;
pub mod elements;
mod geiger;
mod icons;
mod message;
mod notice;
pub mod saytext;
pub mod state;
mod status;
pub(crate) mod statusbar;
pub mod text;
mod tfc;
mod tspkg;
mod version;
pub mod vgui;
mod weapons;

use crate::input::{self, Cvar};
use draw::{HudSprites, TsSprites};
use goldsrc::Global;
use goldsrc::client::engine;
use goldsrc::ext::hud::Screen;
use shared::protocol::{MESSAGES, Msg};
use state::{Ctx, Effect, Hud};
use std::cell::{Cell, RefCell};

/// Everything the draw pass needs besides the message state.
#[derive(Default)]
pub struct Gfx {
    pub screen: Screen,
    pub sprites: HudSprites,
    pub ts: TsSprites,
    /// `CHudTrain::m_hSprite`: `sprites/%d_train.spr` (the stock Half-Life sprite).
    pub train: goldsrc::sys::HSPRITE,
    pub loaded: bool,
}

#[derive(Clone, Copy)]
pub struct HudCvars {
    pub drawhistory_time: Cvar,
    pub fastswitch: Cvar,
    pub deathnotice_time: Cvar,
    pub classautokill: Cvar,
    pub takesshots: Cvar,
    pub zoom_sensitivity_ratio: Cvar,
    pub default_fov: Cvar,
    pub capturemouse: Cvar,
    pub saytext_time: Cvar,
    pub centerid: Cvar,
    pub drawnames: Cvar,
}

static HUD: Global<RefCell<Option<Hud>>> = Global(RefCell::new(None));
static GFX: Global<RefCell<Option<Gfx>>> = Global(RefCell::new(None));
static CVARS: Global<Cell<Option<HudCvars>>> = Global(Cell::new(None));
/// `gHUD.m_flTime` and the last frame time.
static TIME: Global<Cell<(f32, f32)>> = Global(Cell::new((0.0, 0.0)));
/// The zoom sensitivity (`m_flMouseSensitivity`, `CHud+0x18`). The eased FOV (`m_iFOV`) is
/// `state.fov`.
static ZOOM_SENS: Global<Cell<f32>> = Global(Cell::new(0.0));
static INTERMISSION: Global<Cell<bool>> = Global(Cell::new(false));
/// `CHud::Redraw`'s `m_flShotTime` (TS 3.0 `0x100df1a8`, an `int`): when `hud_takesshots` takes
/// the intermission screenshot, 0 for none.
static SHOT_TIME: Global<Cell<i32>> = Global(Cell::new(0));

/// `CHud::Redraw`'s screenshot timer (3.0 `0x10021b4e..0x10021b80`, 2.1 `0x10047c5a..`): entering
/// intermission with `hud_takesshots` set stores `ftol(time + 1.0)`; on every redraw, once
/// `(float)shot < time`, `snapshot` is sent and the timer cleared. Returns whether to send it.
fn shot_due(shot: &mut i32, time: f32, entering_with_shots: bool) -> bool {
    if entering_with_shots {
        *shot = draw::ftol(time as f64 + 1.0);
    }
    if *shot != 0 && (*shot as f32) < time {
        *shot = 0;
        return true;
    }
    false
}
/// A ruleset pinned `default_fov`'s value last frame ([`think_fov`]).
static FOV_PINNED: Global<Cell<bool>> = Global(Cell::new(false));
static ATTACK_CONSUMED: Global<Cell<bool>> = Global(Cell::new(false));
static PREV_BITS: Global<Cell<u32>> = Global(Cell::new(0));

/// The HUD's `SrvSett` 4 ruleset id ([`Hud::gameplay`]) without taking the HUD's borrow for
/// long: `None` before the HUD exists, and (rather than a panic) when called while the HUD is
/// already borrowed, where callers have `h.gameplay` themselves.
pub fn gameplay_id() -> Option<i16> {
    HUD.0.try_borrow().ok().and_then(|h| h.as_ref().and_then(|h| h.gameplay))
}

/// Borrow the HUD state. Don't call into the engine in ways that re-enter the client inside `f`.
pub fn with<R>(f: impl FnOnce(&mut Hud) -> R) -> R {
    let r = {
        let mut h = HUD.0.borrow_mut();
        f(h.get_or_insert_with(Hud::default))
    };
    let pending = std::mem::take(&mut *PENDING_SAY.0.borrow_mut());
    for (text, client) in pending {
        say_text_print(&text, client);
    }
    r
}

fn gfx<R>(f: impl FnOnce(&mut Gfx) -> R) -> R {
    let mut g = GFX.0.borrow_mut();
    f(g.get_or_insert_with(Gfx::default))
}

/// The HUD's screen info. A shared borrow, so it works while the draw pass holds the graphics
/// (menus, voice and the spectator HUD ask for it from there); under the one mutable borrow
/// (`vid_init`) it falls back to the engine's.
pub fn screen() -> Screen {
    match GFX.0.try_borrow() {
        Ok(g) => g.as_ref().map_or_else(Screen::default, |g| g.screen.clone()),
        Err(_) => engine().screen(),
    }
}

/// `CHud::m_iFontHeight` (see [`HudSprites::font_height`]), 0 before the first `vid_init`.
pub fn font_height() -> i32 {
    gfx_ref(|g| g.sprites.font_height())
}

/// Runs `f` with a shared borrow of the graphics, or with empty ones before the first
/// `vid_init` (or, defensively, while they are mutably borrowed).
fn gfx_ref<R>(f: impl FnOnce(&Gfx) -> R) -> R {
    match GFX.0.try_borrow() {
        Ok(g) => match g.as_ref() {
            Some(g) => f(g),
            None => f(&Gfx::default()),
        },
        Err(_) => f(&Gfx::default()),
    }
}

pub fn now() -> f32 {
    TIME.0.get().0
}

pub fn cvars() -> HudCvars {
    CVARS.0.get().expect("hud::init")
}

pub fn intermission() -> bool {
    INTERMISSION.0.get()
}

pub fn observer() -> bool {
    with(|h| h.observer)
}

/// `gHUD.m_flMouseSensitivity`: non-zero while zoomed.
pub fn zoom_sensitivity() -> f32 {
    ZOOM_SENS.0.get()
}

// ---------------------------------------------------------------------------------------------
// Message hooks

macro_rules! hooks {
    ($($v:ident),* $(,)?) => {
        /// One message handler per message, dispatching to [`on_server_message`].
        fn hook_for(m: Msg) -> fn(&[u8]) {
            match m {
                $(Msg::$v => {
                    fn f(b: &[u8]) {
                        on_server_message(Msg::$v, b);
                    }
                    f
                })*
            }
        }
    };
}

hooks!(
    SelAmmo,
    CurWeapon,
    Geiger,
    Health,
    Damage,
    Battery,
    Train,
    HudText,
    SayText,
    TextMsg,
    WeaponList,
    ResetHUD,
    InitHUD,
    GameTitle,
    DeathMsg,
    ScoreInfo,
    TeamInfo,
    TeamScore,
    GameMode,
    Motd,
    ServerName,
    AmmoPickup,
    WeapPickup,
    ItemPickup,
    HideWeapon,
    SetFov,
    ShowMenu,
    ScreenShake,
    ScreenFade,
    TsFade,
    AmmoX,
    TeamNames,
    VguiMenu,
    WeaponInfo,
    ClipInfo,
    WStatus,
    ActItems,
    KFuPower,
    TsHealth,
    TsArmor,
    TsState,
    TsCash,
    TsSpace,
    NextStunt,
    PwUp,
    PwUpTime,
    RoundTime,
    Objective,
    ShowObj,
    Wires,
    SrvSett,
    TsPAward,
    TsBTime,
    TsHVoice,
    TsFlag,
    CustomWp,
    Spectator,
    TsMBlur,
    TsMessage,
    TsSlowMo,
    PTakeDam,
    TsFrags,
    PlInfo,
    TsPSys,
    TsExt,
    TsPkg,
);

fn local_index() -> i32 {
    engine().local_player().map_or(0, |p| p.index)
}

fn third_person() -> bool {
    crate::view::is_third_person()
}

/// Feed a locally built `TextMsg` through the normal handler (`CHudTextMessage::MsgFunc_TextMsg`
/// with no message, as the scoreboard's mute notices do).
pub fn print_text_message(dest: u8, text: &str) {
    let t = shared::protocol::hud::TextMsg { dest, strings: vec![text.to_owned()] };
    on_message(Msg::TextMsg, &t.encode());
}

/// TS team colour by team number (`GetPlayerTextColor` 1001fcd0: `iTeamColors[team % count]`).
pub fn team_color(team: i32) -> [i32; 3] {
    status::team_color(team)
}

/// What part of a message's handling runs ([`handle`]). A client extension may block, rewrite or
/// hold a message's display (`crate::ext::message_pre`, issue #295), never its state: the
/// scoreboard, the spectator's death notice and the extensions' game events.
#[derive(Clone, Copy, PartialEq)]
enum Parts {
    Both,
    State,
    Display,
}

/// A user message from the server: `TSExt` goes to the client extensions and `TSPkg` to the
/// plugin offers ([`tspkg`]), neither past the extensions' hooks; every other one past their
/// interception hooks, then to the HUD.
fn on_server_message(m: Msg, buf: &[u8]) {
    match m {
        Msg::TsExt => return crate::ext::on_tsext(buf),
        Msg::TsPkg => return tspkg::on_message(buf),
        _ => {}
    }
    match crate::ext::message_pre(m, buf) {
        crate::ext::Dispatch::Whole => handle(m, buf, Parts::Both),
        crate::ext::Dispatch::Split(display) => {
            handle(m, buf, Parts::State);
            if let Some(d) = display {
                handle(m, &d, Parts::Display);
            }
        }
    }
}

/// The display of a message a client extension held, once released (`crate::ext`): its state
/// was handled when it came.
pub fn display_released(m: Msg, buf: &[u8]) {
    handle(m, buf, Parts::Display);
}

/// A message, all of its handling: the HUD's own (`print_text_message`, `ts_hud_selftest`).
fn on_message(m: Msg, buf: &[u8]) {
    handle(m, buf, Parts::Both);
}

fn handle(m: Msg, buf: &[u8], parts: Parts) {
    let (state, display) = (parts != Parts::Display, parts != Parts::State);
    if !display {
        // Only the messages an extension can intercept come here, and only these parts of
        // them are state (`crate::ext::intercept::message_allow`).
        if m == Msg::DeathMsg {
            crate::menus::on_message(m, buf);
            notice::on_death_msg_state(buf);
        }
        crate::ext::on_hud_message(m, buf);
        return;
    }
    // `MsgFunc_TextMsg` (`0x10034a60`) prints nothing while `AllowedToPrintText` says no.
    if m == Msg::TextMsg && !crate::menus::allowed_to_print_text() {
        return;
    }
    // Effect messages: the render modules own their effect state and parse the raw payload.
    // The engine keeps a single hook per message, so the HUD hook forwards to them.
    match m {
        Msg::TsPSys => crate::effects::on_tspsys(buf),
        Msg::PTakeDam => crate::effects::on_ptakedam(buf),
        Msg::TsFrags => crate::effects::on_tsfrags(buf),
        Msg::TsMBlur => crate::effects::on_tsmblur(buf),
        Msg::TsBTime => crate::effects::on_tsbtime(buf),
        Msg::TsFade => crate::view::on_tsfade(buf),
        _ => {}
    }
    let e = engine();
    if m == Msg::SetFov {
        // TS ignores SetFOV while client weapons predict the FOV (`cl_lw`).
        if e.cvar_float(c"cl_lw") == 0.0 {
            set_fov(shared::protocol::hud::Reader::new(buf).byte());
        }
        return;
    }
    let screen = screen();
    let li = local_index();
    let c = cvars();
    let ctx = Ctx {
        now: now(),
        screen: &screen,
        titles: &text::EngineTitles,
        local_index: li,
        local_name: if li > 0 { saytext::string(&e.player_info(li).name_bytes) } else { String::new() },
        third_person: third_person(),
        thirdperson_switch: input::cvars().cl_thirdpersonswitch.value() == 1.0,
        cl_thirdperson: input::cvars().cl_thirdperson.value(),
        saytext_time: c.saytext_time.value(),
        deathnotice_time: c.deathnotice_time.value(),
        key_binding: &|b| e.key_lookup_binding(b),
    };
    let mut fx = Vec::new();
    let ok = with(|h| {
        let ok = h.apply(m, buf, &ctx, &mut fx);
        sync_shared(h);
        ok
    });
    if !ok {
        e.print(&format!("[ts] malformed {} message ({} bytes)\n", m.name().to_string_lossy(), buf.len()));
    }
    for f in fx {
        apply_effect(f);
    }
    if m == Msg::SrvSett && shared::protocol::hud::SrvSett::decode(buf).is_some_and(|v| v.key == ts_gameplay::SRVSETT_KEY) {
        // The ruleset's weapon tables for what the HUD reads (the buy menu's prices, slots and
        // ammo, the weapon panel, the crosshair's accuracy) from the moment the HUD knows it, not
        // only from the next predicted command (none with `cl_nopred 1`, issue #111).
        shared::weapons::table::set_ruleset_tables(crate::gameplay::hud_gun_rules().tables());
    }
    // The viewport's handlers for the same messages (scoreboard teams, team menu).
    if state {
        crate::menus::on_message(m, buf);
    }
    if m == Msg::ResetHUD {
        // `0x100219c7`: `m_flMouseSensitivity` (`+0x18`) = 0 after the elements' `Reset`.
        ZOOM_SENS.0.set(0.0);
        // `CHudTSWeapons::Reset` (`0x1004cd60`) zeroes the last key bits (`+0x3724`) too.
        PREV_BITS.0.set(0);
    }
    if m == Msg::DeathMsg {
        if state {
            notice::on_death_msg_state(buf);
        }
        notice::on_death_msg_display(buf);
    }
    // Port addition: the client extensions' game events (issue #293).
    if state {
        crate::ext::on_hud_message(m, buf);
    }
}

/// Mirror the values other modules read into `crate::state`.
fn sync_shared(h: &Hud) {
    crate::state::with(|s| {
        s.health = h.last_hp;
        s.dead = h.health == 0;
        s.observer = h.observer;
        s.weapon = h.weapon;
        s.menu_open = h.menu_open || h.menu.displayed;
        for ((p, items), laser) in h.players.iter().zip(s.active_items.iter_mut()).zip(s.laser_color.iter_mut()) {
            *items = p.active_items;
            *laser = p.laser_color;
        }
        for (spec, p) in s.spectator.iter_mut().zip(h.players.iter().skip(1)) {
            *spec = p.observer;
        }
    });
}

fn apply_effect(f: Effect) {
    let e = engine();
    match f {
        Effect::GameplayRuleset(id) => e.print(&format!("[ts] gameplay: the server runs ruleset {id}\n")),
        Effect::ServerVersion(v) => version::apply(v),
        Effect::Sound(name) => e.play_sound(name, 1.0),
        // At `gHUD.m_vecOrigin`, the client data origin (`0x1004bf6e`).
        Effect::GoSlow => {
            let origin = with(|h| h.vec_origin);
            e.ev_play_sound(-1, origin, 0, "goslow.wav", 1.0, 0.8, 0, 100);
        }
        Effect::ToggleThirdPerson => crate::view::toggle_third_person(),
        Effect::OpenBuyMenu => crate::menus::open_buy(),
        Effect::VguiMenu(id, map_name) => crate::menus::vgui_menu_message(id, map_name),
        Effect::OpenMotd => crate::menus::open_motd(),
        Effect::ResetSpread => crate::prediction::reset_spread(),
        Effect::Fx(fx) => crate::state::with(|s| {
            if s.ts_effects.len() < 256 {
                s.ts_effects.push(fx);
            }
        }),
        Effect::Fade(f) => {
            let st = crate::state::TsFadeState {
                duration: f.duration as f32 / 4096.0,
                hold: f.hold as f32 / 4096.0,
                flags: f.flags,
                rgba: [f.r, f.g, f.b, f.a],
                start: now(),
            };
            crate::state::with(|s| s.ts_fade = Some(st));
        }
        // The texts are the port's Latin-1 strings: the bytes go to the engine as they came.
        Effect::ConsolePrint(t) => e.print_bytes(&saytext::bytes(&t)),
        Effect::SayText(t, client) => say_text_print(&saytext::bytes(&t), client),
        // SDK CHudTextMessage sends HUD_PRINTCENTER to the engine's centre print.
        // Port addition (issue #314): a client extension may hide the centre prints; the engine
        // draws them, so a hidden one isn't printed at all.
        Effect::CenterPrint(t) => {
            if !elements::hidden(ts_client_ext_api::hud::CENTRE) {
                e.center_print_bytes(&saytext::bytes(&t))
            }
        }
    }
}

/// `CHud::MsgFunc_Logo` (`0x100209c0`): `m_iLogo` (`+0x8`) = the byte. While it is set,
/// `CHud::Redraw` draws the animated `sprites/%d_logo.spr` ([`draw_logo`]).
fn msg_logo(buf: &[u8]) {
    let v = shared::protocol::hud::Reader::new(buf).byte();
    with(|h| h.logo = v as i32);
}

/// `CHud::MsgFunc_Concuss` (`0x10021a40`, hooked at `0x1001fe40`): `m_iConcuss` (`+0x1c`) = the
/// byte, which `CHud::UpdateClientData` copies to `v_idlescale`; non-zero shows the `dmg_concuss`
/// status icon in (255, 160, 0), zero hides it. TS servers never send it (a TFC message).
fn msg_concuss(buf: &[u8]) {
    let v = shared::protocol::hud::Reader::new(buf).byte() as i32;
    let icon = with(|h| {
        h.concuss = v;
        if v != 0 {
            return true;
        }
        h.icons.disable(b"dmg_concuss");
        false
    });
    if icon {
        let grenade = gfx_ref(|g| with(|h| h.icons.enable(b"dmg_concuss", [255, 160, 0], &g.sprites)));
        if grenade {
            play_timer_sound();
        }
    }
}

/// `CHudStatusIcons::MsgFunc_StatusIcon` (`0x1002f1a0`): byte on/off, string sprite name, and
/// when on the colour bytes; turning one on also sets the element's `HUD_ACTIVE`.
fn msg_status_icon(buf: &[u8]) {
    let mut r = shared::protocol::hud::Reader::new(buf);
    let on = r.byte();
    let name = saytext::bytes(&r.string());
    if on == 0 {
        with(|h| h.icons.disable(&name));
        return;
    }
    let rgb = [r.byte(), r.byte(), r.byte()];
    let grenade = gfx_ref(|g| {
        with(|h| {
            let gren = h.icons.enable(&name, rgb, &g.sprites);
            h.icons.active = true;
            gren
        })
    });
    if grenade {
        play_timer_sound();
    }
}

/// `EnableIcon`'s grenade-timer cue: `weapons/timer.wav` on the local player, `CHAN_STATIC`,
/// volume 1, attenuation 0.8, pitch 100.
fn play_timer_sound() {
    let e = engine();
    if let Some(p) = e.local_player() {
        let (index, origin) = (p.index, p.origin);
        e.ev_play_sound(index, origin, 6, "weapons/timer.wav", 1.0, 0.8, 0, 100);
    }
}

/// `CHud::m_iConcuss` (`+0x1c`): `CHud::UpdateClientData` (`0x10024b63`) sets `v_idlescale` from
/// it.
pub fn concussion() -> f32 {
    with(|h| h.concuss) as f32
}

// ---------------------------------------------------------------------------------------------
// Init

fn register_cvars() -> HudCvars {
    HudCvars {
        drawhistory_time: Cvar::register(c"hud_drawhistory_time", c"5", 0),
        fastswitch: Cvar::register(c"hud_fastswitch", c"0", 1),
        deathnotice_time: Cvar::register(c"hud_deathnotice_time", c"6", 0),
        classautokill: Cvar::register(c"hud_classautokill", c"1", 3),
        takesshots: Cvar::register(c"hud_takesshots", c"0", 1),
        zoom_sensitivity_ratio: Cvar::register(c"zoom_sensitivity_ratio", c"1.2", 0),
        // `0x100203de`: registered as `sprintf("%i", 80)`, flags 0, then `value = 80` written
        // directly (the string is left as it is).
        default_fov: {
            let c = Cvar::register(c"default_fov", c"80", 0);
            c.set_value(80.0);
            c
        },
        capturemouse: Cvar::register(c"hud_capturemouse", c"1", 1),
        saytext_time: Cvar::register(c"hud_saytext_time", c"5", 0),
        centerid: Cvar::register(c"hud_centerid", c"0", 1),
        drawnames: Cvar::register(c"cl_drawnames", c"1", 1),
    }
}

/// `cancelselect` (`UserCmd_Close` `0x1001a4e0`, the engine's hard binding for Escape): closes
/// the weapon menu with `common/wpn_hudoff.wav`, otherwise sends `escape` to the engine, which
/// opens the main menu. On Steam GoldSrc and FWGS 0.21 that is the only way Escape reaches the
/// menu in game: the engine runs the key's binding instead of opening the menu itself (newer
/// FWGS opens it before the binding).
fn cancel_select() {
    let was_open = with(|h| std::mem::replace(&mut h.menu_open, false));
    crate::events::dev_key_log(|| format!("cancelselect: {}", if was_open { "weapon menu closed" } else { "escape" }));
    if was_open {
        engine().play_sound("common/wpn_hudoff.wav", 1.0);
    } else {
        engine().client_cmd("escape");
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

/// The viewport commands `CHud::Init` registers (`0x1001a710` area, `+commandmenu` `0x1001fe80`,
/// `-commandmenu` `0x1001fea0`, `ForceCloseCommandMenu` `0x1001feb0`, `togglebrowser`
/// `0x1001fec0`). They are registered so configs that bind them don't print "Unknown command",
/// but do nothing:
/// * `ShowCommandMenu` (`0x100635e0`) acts only when the local player has a class
///   (`g_iPlayerClass`, the client data's `playerclass`), which TS's server never sets for a
///   player (only `CBreakable` writes `pev->playerclass`), and TS ships no `commandmenu.txt`, so
///   its menu would be empty anyway. `-commandmenu` and `ForceCloseCommandMenu`
///   (`0x100636a0`, `0x100636d0`) only close that menu.
/// * `togglebrowser` (`ToggleServerBrowser` `0x10063590`) shows or hides TS's VGUI1 server
///   browser panel, which the port doesn't have (see client-hud.md §8).
fn viewport_command_stub() {}

fn register_commands() {
    cmd!(c"cancelselect", cancel_select());
    cmd!(c"+commandmenu", viewport_command_stub());
    cmd!(c"-commandmenu", viewport_command_stub());
    cmd!(c"ForceCloseCommandMenu", viewport_command_stub());
    cmd!(c"special", engine().server_cmd("_special"));
    cmd!(c"togglebrowser", viewport_command_stub());
    cmd!(c"slot1", select_slot(1));
    cmd!(c"slot2", select_slot(2));
    cmd!(c"slot3", select_slot(3));
    cmd!(c"slot4", select_slot(4));
    cmd!(c"slot5", select_slot(5));
    cmd!(c"slot6", select_slot(6));
    cmd!(c"slot7", select_slot(7));
    cmd!(c"slot8", select_slot(8));
    cmd!(c"slot9", select_slot(9));
    cmd!(c"slot10", select_slot(10));
    cmd!(c"invnext", select_next_prev(true));
    cmd!(c"invprev", select_next_prev(false));
    cmd!(c"buy", cmd_buy());
    cmd!(c"ts_hud_selftest", selftest());
}

/// `ts_hud_selftest`: feed a set of encoded TS messages through the real hooks so the HUD can be
/// checked on a server that doesn't send them yet (development aid; not in TS).
fn selftest() {
    use shared::protocol::hud as p;
    let li = local_index().max(1) as u8;
    // `ts_hud_selftest dead`: just report health 0 (the buy menu opens only when dead).
    if engine().cmd_argv(1) == c"dead" {
        on_message(Msg::TsHealth, &p::TsHealth { hp: 0 }.encode());
        return;
    }
    let msgs: Vec<(Msg, Vec<u8>)> = vec![
        (Msg::TsHealth, p::TsHealth { hp: 73 }.encode()),
        (Msg::TsArmor, p::TsArmor { armor: 40 }.encode()),
        (Msg::TsCash, p::TsCash { cash: 1200 }.encode()),
        (Msg::TsSpace, p::TsSpace { slots: 31 }.encode()),
        (Msg::TsState, p::TsState { stance: 1 }.encode()),
        (Msg::SrvSett, p::SrvSett { key: 1, value: 1 }.encode()),
        (Msg::WStatus, p::WStatus { id: 1, owned: 1, clip: 17 }.encode()),
        (Msg::WStatus, p::WStatus { id: 25, owned: 1, clip: 5 }.encode()),
        (Msg::WeaponInfo, p::WeaponInfo { id: 13, clip: 24, reserve: 90, mode: 0, attachments: 8 }.encode()),
        (Msg::PwUp, p::PwUp { flags: 1, value: 3 }.encode()),
        (Msg::PwUp, p::PwUp { flags: 4, value: 0 }.encode()),
        (Msg::PwUpTime, p::PwUpTime { secs: 25 }.encode()),
        (Msg::RoundTime, p::RoundTime { secs: 200, mode: 0 }.encode()),
        (Msg::KFuPower, p::KFuPower { power: 60 }.encode()),
        (Msg::TsMessage, p::TsMessage { r: 0, g: 0, b: 0, flags: 1, time: 100, text: "#HUD_Credits selftest (top)".into() }.encode()),
        (Msg::TsMessage, p::TsMessage { r: 255, g: 200, b: 0, flags: 8, time: 100, text: "TSMessage bottom".into() }.encode()),
        (Msg::ScoreInfo, p::ScoreInfo { client: li, frags: 7, deaths: 2, class: 0, team: 0 }.encode()),
        (Msg::TsPAward, p::TsPAward { client: li, award: 10 }.encode()),
        (Msg::DeathMsg, p::DeathMsg { killer: li, victim: li, weapon: "skull".into() }.encode()),
        (Msg::SayText, p::SayText { client: li, text: "\u{2}selftest: hello from SayText\n".into() }.encode()),
    ];
    for (m, b) in msgs {
        on_message(m, &b);
    }
}

pub fn init() {
    CVARS.0.set(Some(register_cvars()));
    // Port addition: the client's version in its userinfo (`FCVAR_USERINFO`, 2), which tells the
    // server this is the port's client (`shared::protocol::port`).
    Cvar::register(shared::protocol::port::USERINFO_KEY_C, shared::protocol::port::VERSION_C, 2);
    version::register();
    let e = engine();
    // GoldSrc (Steam HL) only copies a `FCVAR_USERINFO` cvar into the userinfo when it is set, not
    // when it is registered (FWGS does both), so without a set the key is missing and a 2.1 server
    // refuses the port's client as TS 3.0's. The documented client API sets cvars only from a
    // float (`Cvar_SetValue`), which puts the version as one number in the userinfo at once
    // (`port::numeric_version`, so a version-checking server can read it at a `+connect`); the
    // version string goes in by a command, which runs only after the command line's (`+connect`,
    // `+exec`).
    let numeric = shared::protocol::port::numeric_version(shared::protocol::port::version_numbers());
    e.cvar_set(shared::protocol::port::USERINFO_KEY_C, numeric.unwrap_or(1.0));
    e.client_cmd(&format!("{} {}\n", shared::protocol::port::USERINFO_KEY, shared::protocol::port::VERSION));
    for (m, name, _) in MESSAGES {
        // The engine parses ScreenShake and ScreenFade itself (FWGS cl_parse.c; GoldSrc
        // likewise); TS hooks neither, nor SelAmmo.
        if matches!(m, Msg::ScreenShake | Msg::ScreenFade | Msg::SelAmmo) {
            continue;
        }
        e.hook_user_msg(name, hook_for(*m));
    }
    e.hook_user_msg(c"Logo", msg_logo);
    e.hook_user_msg(c"Concuss", msg_concuss);
    e.hook_user_msg(c"StatusIcon", msg_status_icon);
    tfc::hook();
    statusbar::hook();
    register_commands();
    // `CHudGeiger::Init` (`0x1001f450`) seeds the CRT's `rand`: `srand(time(NULL))`.
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32);
    with(|h| *h = Hud { crt: message::CrtRand(seed), ..Hud::default() });
}

pub fn vid_init() {
    let e = engine();
    let s = e.screen();
    let first = gfx(|g| !g.loaded);
    let sprites = HudSprites::load(s.width);
    let ts = TsSprites::load();
    let train = e.spr_load(&format!("sprites/{}_train.spr", if s.width < 640 { 320 } else { 640 }));
    gfx(|g| {
        g.screen = s;
        g.sprites = sprites;
        g.ts = ts;
        g.train = train;
        g.loaded = true;
    });
    LOGO.0.set(0);
    status::vid_init();
    vgui::vid_init();
    // `CHud::VidInit` (`0x100208a9`) resets only the current scale (`0x100963e8`). The `TSSlowMo`
    // target (`0x1009ff04`) is left alone, so slow motion still active eases back in.
    crate::state::with(|st| st.time_scale = 1.0);
    crate::prediction::reset_alt_grip();
    with(|h| {
        // The two TS elements' VidInits (`0x10049d10`, `0x1004cb50`), not `Hud::reset`: `CHud::VidInit`
        // runs no element's `Reset`, so a `ShowMenu` menu (`CHudMenu::Reset` `0x100285e0`) and
        // `HideWeapon` (`CHudAmmo::Reset` `0x10019ef0`) survive a video-mode change.
        h.reset_status();
        h.vid_init_weapons();
        // CHudTSStatus::VidInit also clears messages, objectives and flags.
        h.messages = vec![state::TsLine::default(); state::MAX_TS_MESSAGES];
        h.objectives = vec![state::Objective::default(); state::MAX_OBJECTIVES];
        h.obj_max = 0;
        h.flags = Default::default();
        h.flag_count = 0;
        h.flag_blink = [(0.0, false); 2];
        // `0x10049d10` also zeroes the credits (`+0x24`) and free slots (`+0x28`).
        h.cash = 0;
        h.slots = 0;
        h.observer = false;
        // `HUD_VidInit`'s `TeamFortressViewport::Initialize` (`0x10060aa0`).
        h.tfc.initialize();
    });
    weapons::reset_think_sounds();
    // `CHudTSWeapons::VidInit` (`0x1004cb50`): the last `Think`'s key bits (`+0x3724`), so a held
    // `+use` counts as a new press.
    PREV_BITS.0.set(0);
    crate::menus::vid_init(first);
}

/// `HUD_Reset` (TS calls `CHud::VidInit` again).
pub fn reset() {
    vid_init();
}

// ---------------------------------------------------------------------------------------------
// Per-frame logic

/// `InterpolateSlowMotion` (`0x10055ae0`): `cur += min(dt*7, 1) * (target - cur)` while the
/// difference exceeds 0.05, else `cur = target`. TS works on the x87 stack and rounds once when
/// storing; f64 is as close as Rust gets.
pub fn approach_timescale(dt: f32, cur: f32, target: f32) -> f32 {
    let d = target as f64 - cur as f64;
    let d = if d.abs() > 0.05f32 as f64 { (dt as f64 * 7.0).min(1.0) * d } else { d };
    (d + cur as f64) as f32
}

/// The slow-motion part of `HUD_Frame` (`0x1001ab09..0x1001ab24`), TS's only time-scale easer:
/// `g_flTimescale` (`0x100963e8`, `state.time_scale`) moves toward the `TSSlowMo` target
/// (`0x1009ff04`, `state.time_scale_target`) by `HUD_Frame`'s argument, the frame time (FWGS
/// `host.frametime`), narrowed to a float and not clamped.
pub fn frame(time: f64) {
    crate::state::with(|s| s.time_scale = approach_timescale(time as f32, s.time_scale, s.time_scale_target));
}

/// `HUD_GetFOV` (`0x100209f0`): the zoom target `CHud::Think` eases toward (`0x100df1a4`,
/// `state.fov_target`). While recording it writes the value to the demo on every call (record
/// type 0); during playback the last recorded value replaces it.
fn get_fov() -> f32 {
    let e = engine();
    let fov = crate::state::with(|s| s.fov_target);
    if e.demo_recording() {
        crate::demo::write_buffer(crate::demo::FOV, &fov.to_le_bytes());
    }
    if e.demo_playing() {
        let fov = crate::demo::recorded_fov();
        crate::state::with(|s| s.fov_target = fov);
        return fov;
    }
    fov
}

/// The element loop at the start of `CHud::Think` (`0x10049760`): every `HUD_ACTIVE` element's
/// `Think`, with `m_flTimeDelta` (the last redraw's, [`time_delta`]). Of TS's elements only the
/// weapon element's has a body ([`think`]); its Init and Reset set `HUD_ACTIVE` and nothing
/// clears it. Runs before [`think_fov`], from `HUD_UpdateClientData`.
pub fn think_elements(origin: [f32; 3]) {
    with(|h| h.vec_origin = origin);
    think(time_delta());
    with(|h| sync_shared(h));
}

/// The rest of `CHud::Think` (`0x10049760`), from `CHud::UpdateClientData` (`0x10024b00`, i.e.
/// `HUD_UpdateClientData`): eases `m_iFOV` (`state.fov`) toward `HUD_GetFOV` (or `default_fov`
/// while that is 0) with `UTIL_ApproachFOV` ([`crate::view::ease_fov`]) and sets the zoom
/// sensitivity. `ft` is the last refdef's frame time. Returns the eased FOV. A ruleset may pin
/// `default_fov` first and choose when the easer snaps
/// ([`ClientRules::pinned_default_fov`](crate::gameplay::ClientRules::pinned_default_fov),
/// [`fov_snaps`](crate::gameplay::ClientRules::fov_snaps)).
pub fn think_fov(ft: f32) -> f32 {
    let c = cvars();
    let rules = crate::gameplay::current();
    match rules.pinned_default_fov() {
        Some(v) => {
            c.default_fov.set_value(v);
            FOV_PINNED.0.set(true);
        }
        // Port addition: after a map of a ruleset that pinned it, the cvar's value
        // comes back from its string (the engine's `atof`), so the pin doesn't carry over.
        None if FOV_PINNED.0.replace(false) => {
            let s = c.default_fov.string().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            c.default_fov.set_value(crate::spectator::atof(&s));
        }
        None => {}
    }
    let df = c.default_fov.value();
    let default = if df < 0.001 { 80.0 } else { df.clamp(70.0, 110.0) };
    let want = get_fov() as i32;
    let target = if (want as f32) < 0.001 { default as i32 } else { want };
    let mut cur = crate::state::with(|s| crate::view::ease_fov(s.fov, target as f32, ft, rules.fov_snaps(s.fov as i32)));
    let sens = if (cur - default).abs() >= 0.01 {
        mouse_sensitivity(
            rules.dll_build(),
            want as f64 / default as f64,
            input::cvars().sensitivity.value(),
            c.zoom_sensitivity_ratio.value(),
        )
    } else {
        0.0
    };
    ZOOM_SENS.0.set(sens);
    if cur < 0.001 {
        cur = default as i32 as f32;
    }
    crate::state::with(|s| s.fov = cur);
    cur
}

/// The tail of `CHud::MsgFunc_SetFOV` (`0x10049570`), reached only while `cl_lw` is off: the
/// zoom target takes the new FOV, `m_iFOV` jumps to it (0 means 80), and the zoom sensitivity
/// becomes `fov * 0.0125 * sensitivity * zoom_sensitivity_ratio` (0 at 80).
fn set_fov(fov: u8) {
    let fov = fov as i32;
    let cur = if fov == 0 { 80 } else { fov };
    crate::state::with(|s| {
        s.fov_target = fov as f32;
        s.fov = cur as f32;
    });
    let fov_ratio = match crate::gameplay::current().dll_build() {
        crate::gameplay::DllBuild::Optimised => fov as f64 * 0.0125f32 as f64,
        crate::gameplay::DllBuild::Debug => fov as f64 / 80.0,
    };
    let sens = if cur == 80 {
        0.0
    } else {
        mouse_sensitivity(
            crate::gameplay::current().dll_build(),
            fov_ratio,
            input::cvars().sensitivity.value(),
            cvars().zoom_sensitivity_ratio.value(),
        )
    };
    ZOOM_SENS.0.set(sens);
}

/// `m_flMouseSensitivity = fov_ratio × sensitivity × zoom_sensitivity_ratio`, stored as a float:
/// 3.0 keeps the product on the x87 (`0x1004986e`, `0x10049606`); 2.1 stores `fov_ratio ×
/// sensitivity` to a float first (`0x10084b96`, `0x100847a5`). `fov_ratio` is the FOV over the
/// default as the caller worked it out (unrounded).
fn mouse_sensitivity(build: crate::gameplay::DllBuild, fov_ratio: f64, sensitivity: f32, zoom_ratio: f32) -> f32 {
    let t = fov_ratio * sensitivity as f64;
    match build {
        crate::gameplay::DllBuild::Optimised => (t * zoom_ratio as f64) as f32,
        crate::gameplay::DllBuild::Debug => (zoom_ratio as f64 * t as f32 as f64) as f32,
    }
}

/// The TS weapon element's `Think` (1004ce10), the parts that need no key bits: laser colour,
/// kung-fu regeneration, heartbeat/breathing sounds, fire-mode switch sound. The laser colour and
/// the sounds are the ruleset's ([`ClientRules::think_parts`](crate::gameplay::ClientRules::think_parts)).
/// Run from [`think_elements`].
fn think(dt: f32) {
    let e = engine();
    let ic = input::cvars();
    let laser = ic.cl_lasercolor.value() as i32;
    let (ts, slowmo) = crate::state::with(|s| (s.time_scale, s.slowmo_factor100));
    // `0x1004ce58`: `m_flTimeDelta / (1 / max(ts, 0.01))`, so both run slower in slow motion.
    let step = dt * ts.max(0.01);
    let alive = !dead_or_observer();
    let now = now();
    let (parts, send_laser, firemode_changed, kfu_power) = with(|h| {
        let rules = crate::gameplay::hud_rules(h.gameplay);
        let parts = rules.think_parts();
        let send = h.last_laser_color != laser;
        h.last_laser_color = laser;
        h.next_stunt = (h.next_stunt - step).max(0.0);
        h.regen_kungfu(rules, step, now, slowmo);
        // `0x1004d25d`: dead or observing skips the check without taking the new mode, so a
        // change made meanwhile still clicks on respawn.
        let fm = alive && h.weapon > 0 && h.last_firemode != h.firemode;
        if fm {
            h.last_firemode = h.firemode;
        }
        (parts, send, fm, h.kfu_power)
    });
    // `0x1004cf5e`: the copy the predicted kung-fu block gates on.
    crate::state::with(|s| s.kfu_power = kfu_power);
    // A ruleset without these parts (`ClientRules::think_parts`) still tracks the laser colour and
    // fire mode; the next `Reset` sets the colour to −1, so a map with the send sends it afresh.
    if send_laser && parts.laser_color {
        e.server_cmd(&format!("lasclr_{laser}"));
    }
    if firemode_changed && parts.firemode_click {
        // `0x1004d35e..0x1004d3cd`: on the local player, channel 3 (`CHAN_ITEM`), volume
        // `RandomFloat(0.92, 1)`, attenuation 1.25, pitch `RandomLong(-5, 5) + 25 - ftol(ts × -75)`.
        let r = goldsrc::ext::render::random_long(-5, 5);
        let pitch = r.wrapping_add(25i32.wrapping_sub(draw::ftol(ts as f64 * -75.0)));
        let vol = goldsrc::ext::render::random_float(0.92, 1.0);
        if let Some(me) = e.local_player() {
            let (index, origin) = (me.index, me.curstate.origin);
            e.ev_play_sound(index, origin, 3, "weapons/switch.wav", vol, 1.25, 0, pitch);
        }
    }
    if parts.sounds {
        weapons::think_sounds(dt);
    }
}

pub fn dead_or_observer() -> bool {
    let demo = engine().demo_playing() || engine().demo_recording();
    with(|h| (!demo && h.health == 0) || h.observer)
}

/// A new `+use` press with `cl_usepowerup` on (`0x1004ce89..0x1004cee9`; 2.1 `0x10089a14..0x10089a7a`
/// is the same): with a stored power-up (`CHudTSStatus+0x14`, `pw_flags`) the press sends
/// `usepowerup` only while `time <= window` (`fld [+0x3720]; fcomp time`, C0 skips), then sets
/// the window to `time + 0.3`. So it takes a double tap within 0.3 s; a single press only arms
/// the window. Returns whether to send.
fn usepowerup_tap(h: &mut Hud, t: f32) -> bool {
    if h.pw_flags == 0 {
        return false;
    }
    let send = t <= h.usepowerup_time;
    // `now + 0.3` with the double 0.3 (`0x1004cee3`, 2.1 `0x10089a6b`), stored as a float.
    h.usepowerup_time = (t as f64 + 0.3) as f32;
    send
}

/// Called from `CL_CreateMove` with this frame's button bits: the weapon menu takes `+attack` as
/// select (`weapon_%i`) and `+attack2` as drop (`drop_%i`); `+use` triggers `usepowerup` when
/// `cl_usepowerup` is on. Returns the bits the HUD consumed.
pub fn weapon_menu_buttons(bits: u32) -> u32 {
    use goldsrc::sys::{IN_ATTACK, IN_ATTACK2, IN_USE};
    let prev = PREV_BITS.0.replace(bits);
    let pressed = bits & !prev;
    let e = engine();
    let t = now();
    // cl_usepowerup: a double tap of +use with a stored power-up sends `usepowerup`.
    if input::cvars().cl_usepowerup.value() != 0.0 && pressed & IN_USE != 0 && with(|h| usepowerup_tap(h, t)) {
        e.server_cmd("usepowerup");
    }
    let mut consumed = 0;
    ATTACK_CONSUMED.0.set(false);
    if dead_or_observer() || crate::menus::buy_open() {
        return 0;
    }
    // `0x1004d263..0x1004d339`: with the menu open, attack and attack2 are each tested (and
    // taken) on their own, so both can go out in the same frame.
    let cmds = with(|h| {
        let mut out = Vec::new();
        if !h.menu_open {
            return out;
        }
        let slot = h.grid.get(h.menu_row as usize).and_then(|r| r.get(h.menu_bucket as usize)).copied();
        let id = slot.map_or(-1, |s| s.id);
        if bits & IN_ATTACK != 0 {
            consumed |= IN_ATTACK;
            h.menu_open = false;
            if id >= 0 {
                out.push(format!("weapon_{id}"));
            }
        }
        if bits & IN_ATTACK2 != 0 {
            consumed |= IN_ATTACK2;
            h.menu_open = false;
            if id >= 0 {
                out.push(format!("drop_{id}"));
            }
        }
        out
    });
    for c in cmds {
        e.server_cmd(&c);
    }
    if consumed & IN_ATTACK != 0 {
        ATTACK_CONSUMED.0.set(true);
    }
    consumed
}

pub fn weapon_menu_consumed_attack() -> bool {
    ATTACK_CONSUMED.0.get()
}

/// `slot1..10` → `TSWeapons::SelectSlot` (1004f220).
pub fn select_slot(n: i32) {
    // An open CHudMenu takes keys 1..10 first (`0x1004f220`), whether or not the slot is one of
    // its options: `SelectMenuItem` ignores an invalid one and the key goes no further.
    if (1..=10).contains(&n) && with(|h| h.menu.displayed) {
        if let Some(cmd) = with(|h| h.menu_select(n)) {
            engine().server_cmd(cmd.trim_end());
        }
        return;
    }
    if crate::menus::buy_open() {
        crate::menus::buy_key(n);
        return;
    }
    if crate::menus::team_menu_key(n) {
        return;
    }
    if dead_or_observer() {
        return;
    }
    let fast = cvars().fastswitch.value() != 0.0;
    if let Some(cmd) = with(|h| weapons::select_slot(h, n, fast)) {
        engine().server_cmd(&cmd);
    }
}

/// `invnext`/`invprev` (1004fce0/1004fd40): 0.1 s debounce.
fn select_next_prev(next: bool) {
    if dead_or_observer() {
        return;
    }
    let t = now();
    let fast = cvars().fastswitch.value() != 0.0;
    let cmd = with(|h| {
        if h.invnext_time > t {
            return None;
        }
        // `now + 0.1` with the double 0.1 (`0x1004fd1f`, 2.1 `0x1008da11`), stored as a float.
        h.invnext_time = (t as f64 + 0.1) as f32;
        weapons::select_next_prev(h, next, fast)
    });
    if let Some(c) = cmd {
        engine().server_cmd(&c);
    }
}

/// `buy` (1004fda0): only when dead or observing, and not while the weapon menu is open.
fn cmd_buy() {
    if !dead_or_observer() {
        return;
    }
    if with(|h| h.menu_open) {
        return;
    }
    crate::menus::open_buy();
}

// ---------------------------------------------------------------------------------------------
// Drawing

/// Per-frame draw context.
pub struct Dc<'a> {
    pub g: &'a Gfx,
    pub now: f32,
    pub hud: [i32; 3],
}

impl Dc<'_> {
    pub fn w(&self) -> i32 {
        self.g.screen.width
    }
    pub fn h(&self) -> i32 {
        self.g.screen.height
    }
    pub fn string(&self, x: i32, y: i32, t: &str, rgb: [i32; 3]) -> i32 {
        text::draw_string(&self.g.screen, x, y, self.g.screen.width, t, rgb)
    }
    pub fn string_len(&self, t: &str) -> i32 {
        text::string_len(&self.g.screen, t)
    }
    pub fn number(&self, x: i32, y: i32, flags: u8, n: i32, rgb: [i32; 3]) -> i32 {
        draw::draw_number(&self.g.sprites, x, y, flags, n, rgb)
    }
    pub fn fill(&self, x: i32, y: i32, w: i32, h: i32, rgb: [i32; 3], a: i32) {
        engine().fill(x, y, w, h, rgb, a);
    }
    /// Draws a ruleset's layout ([`draw::HudOp`]).
    pub fn ops(&self, ops: &[draw::HudOp]) {
        for op in ops {
            match *op {
                draw::HudOp::Fill { x, y, w, h, rgb, a } if w > 0 && h > 0 => self.fill(x, y, w, h, rgb, a),
                draw::HudOp::Fill { .. } => {}
                draw::HudOp::Text { x, y, text, rgb } => {
                    self.string(x, y, text, rgb);
                }
                draw::HudOp::Label { x, y, ref text, rgb } => {
                    self.string(x, y, text, rgb);
                }
                draw::HudOp::Number { x, y, flags, n, rgb } => {
                    self.number(x, y, flags, n, rgb);
                }
                draw::HudOp::Sprite { sprite: draw::HudSprite::ItemInstalled, x, y, rgb } => self.spr(self.g.ts.item_installed, x, y, rgb),
            }
        }
    }
    pub fn spr(&self, h: goldsrc::sys::HSPRITE, x: i32, y: i32, rgb: [i32; 3]) {
        if h != 0 {
            let e = engine();
            e.spr_set(h, rgb);
            e.spr_draw_additive(0, x, y, None);
        }
    }
}

/// Run `f` with a draw context (for `menus`, which draws after the HUD).
pub fn with_dc(f: impl FnOnce(&Dc)) {
    let g = GFX.0.borrow();
    let Some(g) = g.as_ref() else { return };
    if !g.loaded {
        return;
    }
    let dc = Dc { g, now: now(), hud: input::hud_rgb() };
    f(&dc);
}

/// `gHUD.m_flTimeDelta` (`0x100cf950`): the time between the last two `HUD_Redraw`s, never
/// negative (the port also caps it at 1 s, which only matters for the HUD's own timers).
pub fn time_delta() -> f32 {
    TIME.0.get().1
}

pub fn redraw(time: f32, intermission: bool) {
    let e = engine();
    let (old, _) = TIME.0.get();
    let dt = (time - old).clamp(0.0, 1.0);
    TIME.0.set((time, dt));

    // Intermission: TS hides VGUI menus and shows the scoreboard (and with hud_takesshots times a
    // `snapshot`, [`shot_due`]); on leaving it hides the scoreboard.
    // Port addition: the client extensions' HUD control (issue #314, `elements`).
    elements::next_frame();
    let was = INTERMISSION.0.replace(intermission);
    let entering = intermission && !was;
    if intermission != was {
        crate::menus::intermission(intermission);
    }
    let mut shot = SHOT_TIME.0.get();
    if shot_due(&mut shot, time, entering && cvars().takesshots.value() != 0.0) {
        e.client_cmd("snapshot\n");
    }
    SHOT_TIME.0.set(shot);
    with(|h| h.intermission = intermission);
    let hide_all = with(|h| h.hide_hud & 4 != 0);

    let s = e.screen();
    gfx(|g| {
        if s.width != g.screen.width || s.height != g.screen.height {
            g.sprites = HudSprites::load(s.width);
        }
        g.screen = s;
    });
    with(|h| sync_shared(h));
    let g = GFX.0.borrow();
    let Some(g) = g.as_ref() else { return };
    let dc = Dc { g, now: time, hud: input::hud_rgb() };
    use ts_client_ext_api::hud as el;
    if intermission {
        // Only `HUD_INTERMISSION` elements draw: TS sets it on the say text alone.
        elements::draw(el::CHAT, || draw_say_text(&dc));
    } else if !hide_all {
        let nohud = input::cvars().nohud.value() != 0.0;
        // TS element order (§4.1), each needing `HUD_ACTIVE` and no `HIDEHUD_ALL`: CHudGeiger,
        // CHudTrain, CHudMessage, CHudStatusBar, CHudDeathNotice, CHudStatusIcons, CHudSpectator
        // (HLTV name tags), CHudTSWeapons, CHudTSStatus, CHudSayText, CHudMenu.
        if let Some((sample, vol)) = with(|h| {
            if h.geiger_active {
                geiger::draw(crate::gameplay::hud_rules(h.gameplay).dll_build(), h.geiger_range, &mut h.crt)
            } else {
                None
            }
        }) {
            e.play_sound(&sample, vol);
        }
        draw_train(&dc);
        let mut canvas = draw::EngineCanvas(&g.sprites);
        elements::draw(el::HUDTEXT, || {
            with(|h| {
                if h.message.active {
                    h.message.build = crate::gameplay::hud_rules(h.gameplay).dll_build();
                    h.message.draw(time, &g.screen, &g.sprites, &mut canvas, &mut h.crt)
                }
            })
        });
        statusbar::draw(&dc);
        if !nohud {
            elements::draw(el::KILLFEED, || notice::draw_death_notices(&dc));
        }
        let icons = with(|h| h.icons.active.then(|| h.icons.clone()));
        if let Some(icons) = icons {
            elements::draw(el::STATUS_ICONS, || icons.draw(dc.h()));
        }
        elements::draw(el::SPECTATOR, crate::spectator::draw_hltv_names);
        weapons::draw(&dc, nohud);
        status::draw(&dc, nohud);
        elements::draw(el::CHAT, || draw_say_text(&dc));
        notice::draw_menu(&dc);
    }
    draw_logo(g, time);
}

/// The end of `CHud::Redraw` (`0x10021bcd`): while `m_iLogo` is set, frame
/// `grgLogoFrame[(int)(time * 20) % 56] - 1` of `sprites/%d_logo.spr` (loaded on first use, and
/// tried again every frame while missing) in (250, 250, 250) at the top right, half its height
/// down. Drawn in intermission and under `HIDEHUD_ALL` too.
fn draw_logo(g: &Gfx, time: f32) {
    /// `grgLogoFrame` (`0x10093990`).
    const FRAMES: [i32; 56] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 13, 13, 13, 13, 13, 12, 11, 10, 9, 8, 14, 15, 16, 17, 18, 19, 20, 20, 20, 20, 20, 20,
        21, 22, 23, 24, 25, 26, 27, 28, 29, 29, 29, 29, 29, 29, 28, 27, 26, 25, 24, 30, 31,
    ];
    if with(|h| h.logo) == 0 {
        return;
    }
    let e = engine();
    let mut spr = LOGO.0.get();
    if spr == 0 {
        // `0x10058450`: `sprites/%d_logo.spr` with 640 from 640 px wide, else 320.
        spr = e.spr_load(&format!("sprites/{}_logo.spr", if g.screen.width >= 640 { 640 } else { 320 }));
        LOGO.0.set(spr);
    }
    e.spr_set(spr, [250, 250, 250]);
    let x = g.screen.width - e.spr_width(spr, 0);
    let y = e.spr_height(spr, 0) / 2;
    // `ftol(time × 20.0f)` off the x87, unrounded (`0x10021c21`).
    let i = (draw::ftol(time as f64 * 20.0) % 56).unsigned_abs() as usize;
    e.spr_draw_additive(FRAMES[i] - 1, x, y, None);
}

/// `CHud::m_hsprLogo` (`+0x4`), zeroed by `VidInit`.
static LOGO: Global<Cell<goldsrc::sys::HSPRITE>> = Global(Cell::new(0));

/// The say text element, when active (`HUD_ACTIVE`).
fn draw_say_text(dc: &Dc) {
    if !with(|h| h.say_text.active) {
        return;
    }
    // `Draw` (`0x1002eb40`) draws nothing while `AllowedToPrintText` says no.
    if !crate::menus::allowed_to_print_text() {
        return;
    }
    let plan = with(|h| h.say_text.draw_plan(dc.now, &mut EngineSay));
    let e = engine();
    for l in plan {
        match l {
            saytext::DrawLine::Name { y, name, color, rest } => {
                let x = e.draw_console_bytes(10, y, Some(color), &name);
                e.draw_console_bytes(x, y, None, &rest);
            }
            saytext::DrawLine::Plain { y, text } => {
                e.draw_console_bytes(10, y, crate::gameplay::current().say_text_style().plain_color, &text);
            }
        }
    }
}

/// The engine half of [`saytext::SayEnv`].
struct EngineSay;
impl saytext::SayEnv for EngineSay {
    fn string_size(&self, text: &[u8]) -> (i32, i32) {
        engine().console_bytes_size(text)
    }
    fn console_print(&mut self, text: &[u8]) {
        engine().print_bytes(text);
    }
}

/// Lines [`say_text_print`] couldn't take because the HUD state was borrowed (an effect that
/// warns from inside a HUD callback); [`with`] adds them when the borrow ends.
static PENDING_SAY: Global<RefCell<Vec<PendingSay>>> = Global(RefCell::new(Vec::new()));
/// A deferred `SayTextPrint`: (text, client).
type PendingSay = (Vec<u8>, i32);

/// `CHudSayText::SayTextPrint` (`0x1002ed00`): drops text from a muted player (when the
/// ruleset's [`SayTextStyle`](saytext::SayTextStyle) says so); while `AllowedToPrintText` says
/// no it goes to the console only; otherwise it becomes a chat line (the speaker's name coloured
/// when the text starts with `\x02`) and `misc/talk.wav` plays.
pub fn say_text_print(text: &[u8], client: i32) {
    let Ok(mut h) = HUD.0.try_borrow_mut() else {
        PENDING_SAY.0.borrow_mut().push((text.to_vec(), client));
        return;
    };
    let h = h.get_or_insert_with(Hud::default);
    if crate::gameplay::hud_rules(h.gameplay).say_text_style().mute_blocked && crate::voice::is_player_blocked(client) {
        return;
    }
    let e = engine();
    if !crate::menus::allowed_to_print_text() {
        e.print_bytes(text);
        return;
    }
    let name = (text.first() == Some(&2) && client > 0).then(|| e.player_info(client).name_bytes);
    // `GetClientColor` reads the scoreboard teams, which live in the state borrowed here.
    let color = name.as_ref().and_then(|_| notice::client_color(&h.players, local_index(), client));
    // `ScreenWidth`/`ScreenHeight` (the HUD's screen info); the draw pass may hold it.
    let screen = match GFX.0.try_borrow() {
        Ok(g) => g.as_ref().map(|g| (g.screen.width, g.screen.height)),
        Err(_) => None,
    }
    .unwrap_or_else(|| {
        let s = e.screen();
        (s.width, s.height)
    });
    let speaker = name.as_deref().filter(|n| !n.is_empty()).map(|n| (n, color));
    let saytime = || cvars().saytext_time.value();
    h.say_text.print(text, speaker, now(), saytime, screen, &mut EngineSay);
    e.play_sound("misc/talk.wav", 1.0);
}

/// `CHudTrain::Draw` (10034d40): the speed notch frame of the train sprite, yellowish, a third of
/// the way across and just above the bottom text line. Hidden at notch 0 (`HUD_ACTIVE` off) and,
/// like every element in `CHud::Redraw`, under `HIDEHUD_ALL`.
fn draw_train(dc: &Dc) {
    let (pos, hide) = with(|h| (h.train_pos, h.hide_hud));
    let spr = dc.g.train;
    if pos == 0 || hide & 4 != 0 || spr == 0 {
        return;
    }
    let e = engine();
    e.spr_set(spr, [255, 160, 0]);
    // `m_iFontHeight`, the `number_0` height (`0x10034d8f`).
    let y = dc.h() - e.spr_height(spr, 0) - dc.g.sprites.font_height();
    let x = dc.w() / 3 + e.spr_width(spr, 0) / 4;
    e.spr_draw_additive(pos as i32 - 1, x, y, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2.1 stores `fov / default × sensitivity` to a float before `× zoom_sensitivity_ratio`
    /// (`0x10084b96`); 3.0 keeps it on the x87 (`0x1004986e`).
    #[test]
    fn zoom_sensitivity_follows_the_build() {
        use crate::gameplay::DllBuild;
        assert_eq!(mouse_sensitivity(DllBuild::Optimised, 11.0 / 80.0, 1.0, 0.8).to_bits(), 0x3de1_47ae);
        assert_eq!(mouse_sensitivity(DllBuild::Debug, 11.0 / 80.0, 1.0, 0.8).to_bits(), 0x3de1_47af);
    }

    /// #252: `hud_takesshots` waits until `ftol(time + 1)` has passed (3.0 `0x10021b62`, 2.1
    /// `0x10047c5a`), checked on every redraw.
    #[test]
    fn intermission_snapshot_waits() {
        let mut shot = 0;
        assert!(!shot_due(&mut shot, 10.7, true));
        assert_eq!(shot, 11, "ftol truncates");
        assert!(!shot_due(&mut shot, 11.0, false), "strictly later");
        assert!(shot_due(&mut shot, 11.01, false));
        assert_eq!(shot, 0);
        assert!(!shot_due(&mut shot, 50.0, false), "once");
        assert!(!shot_due(&mut shot, 0.5, false));
    }

    #[test]
    fn timescale_easing() {
        assert_eq!(approach_timescale(0.016, 1.0, 1.0), 1.0);
        let v = approach_timescale(0.1, 1.0, 0.35);
        assert!((v - (1.0 + 0.7 * (0.35 - 1.0))).abs() < 1e-6);
        assert_eq!(approach_timescale(0.1, 0.38, 0.35), 0.35, "snaps within 0.05");
        assert!((approach_timescale(1.0, 1.0, 0.35) - 0.35).abs() < 1e-6, "rate capped at 1");
        let mut ts = 1.0;
        for _ in 0..200 {
            ts = approach_timescale(0.01, ts, 0.35);
        }
        assert_eq!(ts, 0.35, "settles exactly on the target");
    }

    /// `0x1004cebc` (2.1 `0x10089a48`): `usepowerup` goes out on the second +use press within
    /// 0.3 s, never on the first, and only with a stored power-up.
    #[test]
    fn usepowerup_needs_a_double_tap() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud { pw_flags: 1, ..Hud::default() };
        assert!(!usepowerup_tap(&mut h, 10.0), "the first press only arms the window");
        assert!((h.usepowerup_time - 10.3).abs() < 1e-5);
        assert!(usepowerup_tap(&mut h, 10.2), "a second press within 0.3 s sends");
        assert!(!usepowerup_tap(&mut h, 11.0), "a press after the window only re-arms it");
        let end = h.usepowerup_time;
        assert!(usepowerup_tap(&mut h, end), "the window's end is inclusive (time <= window)");
        h.pw_flags = 0;
        h.usepowerup_time = 0.0;
        assert!(!usepowerup_tap(&mut h, 20.0));
        assert!(!usepowerup_tap(&mut h, 20.1), "no power-up: nothing sent, window untouched");
        assert_eq!(h.usepowerup_time, 0.0);
    }

    #[test]
    fn every_message_has_a_hook() {
        for (m, _, _) in MESSAGES {
            let _: fn(&[u8]) = hook_for(*m); // every message maps to a handler (exhaustive match)
        }
    }
}
