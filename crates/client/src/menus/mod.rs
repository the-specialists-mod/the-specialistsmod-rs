//! TS's menus: the buy menu (`CTSBuyMenu`, which TS draws in immediate mode too) and the VGUI1
//! viewport panels (team menu, MOTD window, scoreboard), which the port paints with engine fills
//! through [`crate::hud::vgui`]. Owner: HUD/UI work package. Reference: decomp
//! docs/client/architecture-hud.md §6.
//!
//! The VGUI menus queue as TS's viewport queues them (`ShowVGUIMenu` `0x10064050`): the first is
//! shown, the others open in turn as the shown one closes (`HideTopMenu` `0x10064200`).
//!
//! The mouse: while a menu shows the cursor, `input` moves a virtual cursor instead of the view
//! and routes clicks here (TS: VGUI's input, or `IN_MouseEvent` → `CTSBuyMenu::MouseEvent`).

pub mod buy;
mod motd;
mod scoreboard;
mod scroll;
mod team;
pub(crate) mod widgets;

use crate::hud::vgui::{self, EngineSurface};
use crate::hud::{self, Dc, draw::half, text::localise};
use buy::{Action, BuyMenu, Rng, Rules, page};
use goldsrc::Global;
use goldsrc::client::{Cvar, engine};
use scoreboard::{PlayerSlot, ScoreBoard};
use shared::protocol::Msg;
use std::cell::RefCell;
use std::ffi::CStr;

/// A queued VGUI menu (`CMenuPanel`, by its `VGUIMenu` id).
#[derive(Debug, Clone, PartialEq)]
enum Menu {
    /// 2: `CTeamMenuPanel` (the viewport's one instance).
    Team,
    /// 4 (the map briefing) or 5 (the MOTD): a `CMessageWindowPanel`, made (and parented to the
    /// viewport, [`vgui::raise`]) when it was asked for.
    Text(i32, Box<motd::Motd>, u64),
}

impl Menu {
    fn id(&self) -> i32 {
        match self {
            Menu::Team => 2,
            Menu::Text(id, ..) => *id,
        }
    }
}

#[derive(Default)]
struct Menus {
    buy: BuyMenu,
    /// Clickable rows from the last draw: (x, y, w, h, page, key).
    rows: Vec<(i32, i32, i32, i32, i32, i32)>,
    /// `m_pCurrentMenu` and its `m_pNextMenu` chain.
    queue: Vec<Menu>,
    team: Option<team::TeamMenu>,
    /// The screen size and VGUI scale the team menu was made for.
    team_size: (i32, i32, Option<crate::gameplay::DllBuild>),
    /// The scoreboard is shown (`+showscores`).
    scores: bool,
    board: ScoreBoard,
    path: String,
    /// The viewport's `m_sMapName` (`+0xa5c`, 64 bytes): `VGUIMenu` 4's map name, or the level's
    /// once a briefing was made from it. `ReInitialize` empties it.
    map_name: Vec<u8>,
    /// `m_iCurrentTeamNumber` (`+0x214`): the local team number the viewport last saw.
    team_number: i32,
}

impl Menus {
    /// `paintBackground`'s team check (`0x100648c0`, TS 2.1 `0x100b1210`): records `team` as
    /// `m_iCurrentTeamNumber`; true when it changed (then TS updates the shown menu).
    fn note_team(&mut self, team: i32) -> bool {
        std::mem::replace(&mut self.team_number, team) != team
    }
}

static MENUS: Global<RefCell<Option<Menus>>> = Global(RefCell::new(None));

fn with<R>(f: impl FnOnce(&mut Menus) -> R) -> R {
    let mut m = MENUS.0.borrow_mut();
    f(m.get_or_insert_with(Menus::default))
}

struct EngineRng;
impl Rng for EngineRng {
    fn long(&mut self, lo: i32, hi: i32) -> i32 {
        goldsrc::ext::render::random_long(lo, hi)
    }
    fn float(&mut self, lo: f32, hi: f32) -> f32 {
        goldsrc::ext::render::random_float(lo, hi)
    }
}

fn rules() -> Rules {
    let (gamemode, restriction, ammocount, cashbased, tscash, gameplay) =
        hud::with(|h| (h.gamemode, h.restriction, h.ammocount, h.cashbased, h.cash, h.gameplay));
    let ruleset = crate::gameplay::hud_rules(gameplay);
    Rules {
        gamemode,
        restriction,
        ammocount,
        buy_kevlar: crate::input::cvars().cl_buykevlar.value() != 0.0,
        cash: buy::cash(ruleset.buy_cash_based(gamemode, cashbased), tscash),
        cash_based: ruleset.buy_cash_based(gamemode, cashbased),
        menu: ruleset.buy_menu(),
    }
}

/// The buy menu's text for `msg`: the ruleset's fixed text ([`buy::MenuRules::text`]) or the
/// engine's titles.
fn buy_text(r: &Rules, msg: &str) -> String {
    buy::text(&r.menu, &hud::text::EngineTitles, msg)
}

pub fn init() {
    // `Scheme_Init` (1005c2a0), right after CHud::Init: the scheme manager (`hud::vgui`) picks
    // bitmap fonts by it.
    Cvar::register(c"bitmapfonts", c"1", 0);
}

/// `FUN_10036480`: the nine saved configs from `path` (`<gamedir>/tsbuylist.tsa`). When it can't
/// be opened for reading, it is created empty (`"wb"`); when that fails too, the path is
/// forgotten (`+0x2c6c` = ""), so nothing is saved until the next `Init`.
fn read_buylist(path: &str) -> ([buy::Config; buy::SAVED_CONFIGS], String) {
    match std::fs::read(path) {
        Ok(bytes) => (buy::parse_buylist(&bytes), path.to_owned()),
        Err(_) => (Default::default(), if std::fs::File::create(path).is_ok() { path.to_owned() } else { String::new() }),
    }
}

/// `HUD_VidInit`'s part here: `CHudTSWeapons::VidInit` (`0x1004cb50`) re-runs the buy menu's
/// `Init` (`0x10036200`, [`BuyMenu::new`]), which re-reads `tsbuylist.tsa` and forgets the
/// current and last configs. Then the viewport's `ReInitialize` (`0x10060aa0`): menus hidden,
/// the team menu's map name and the scoreboard's data reset, the scoreboard hidden.
pub fn vid_init(_first: bool) {
    let (saved, path) = read_buylist(&format!("{}/tsbuylist.tsa", engine().game_directory()));
    with(|m| {
        m.buy = BuyMenu::new(saved);
        m.path = path;
        m.queue.clear();
        if let Some(t) = &mut m.team {
            t.initialize();
        }
        m.board.initialize();
        m.scores = false;
        m.map_name.clear();
    });
    // `Initialize` zeroes `g_iPlayerClass` and `g_iTeamNumber` (`0x10060aa0`).
    crate::state::with(|s| {
        s.player_class = 0;
        s.team_number = 0;
    });
    crate::voice::stop_squelch_mode();
}

/// `FUN_10036560`: write the nine configs. Written to a temporary file and renamed, so a
/// symlinked content file (xtask installs) is replaced rather than written through.
fn save_file() {
    let (path, bytes) = with(|m| (m.path.clone(), buy::write_buylist(&m.buy.saved)));
    // `Init` couldn't open the file, so TS's `fopen("")` fails too.
    if path.is_empty() {
        return;
    }
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
        engine().print(&format!("[ts] couldn't write {path}\n"));
    }
}

fn run_actions(actions: Vec<Action>) {
    let e = engine();
    let r = rules();
    for a in actions {
        match a {
            Action::ServerCmd(c) => e.server_cmd(&c),
            Action::Message(t, rgb) => add_message(&buy_text(&r, &t), rgb),
            Action::MessageArg(t, arg, rgb) => {
                let f = buy_text(&r, &t);
                let arg_i = arg.parse::<i64>().ok();
                let s = hud::text::format(&f, &[arg_i.map_or(hud::text::Arg::S(&arg), hud::text::Arg::I)]);
                add_message(&s, rgb)
            }
            Action::Close => {}
            Action::SaveFile => save_file(),
        }
    }
}

/// `AddTSMessage(text, rgb, 20 tenths, 0)`.
fn add_message(text: &str, rgb: [i32; 3]) {
    let screen = hud::screen();
    let ctx = hud::state::Ctx {
        now: hud::now(),
        screen: &screen,
        titles: &hud::text::EngineTitles,
        local_index: 0,
        local_name: String::new(),
        third_person: false,
        thirdperson_switch: false,
        cl_thirdperson: 0.0,
        saytext_time: 5.0,
        deathnotice_time: 6.0,
        key_binding: &hud::state::no_key_binding,
    };
    hud::with(|h| h.add_ts_message(&ctx, text, rgb, 20, 0));
}

// ---- buy menu API ----------------------------------------------------------------------------

pub fn buy_open() -> bool {
    with(|m| m.buy.active)
}

/// `CTSBuyMenu::Open` (10036670): refused during demo playback (as the ruleset says,
/// [`buy::MenuRules::refused_in_demo`]); centres the cursor.
pub fn open_buy() {
    let r = rules();
    if !buy::opens(&r.menu, engine().demo_playing()) {
        return;
    }
    let opened = with(|m| {
        let was = m.buy.active;
        m.buy.open(&r, &mut EngineRng);
        !was && m.buy.active
    });
    if opened {
        crate::input::center_cursor();
    }
}

pub fn buy_key(key: i32) {
    let r = rules();
    let actions = with(|m| {
        let pg = m.buy.page;
        m.buy.handle_key(pg, key, &r, &mut EngineRng)
    });
    run_actions(actions);
}

// ---- engine data for the VGUI menus ----------------------------------------------------------

/// `GetAllPlayersInfo` (`0x10064880`): the engine's player list for clients 1..63.
fn player_slots() -> Vec<PlayerSlot> {
    let e = engine();
    let mut v = vec![PlayerSlot::default(); 65];
    for (i, slot) in v.iter_mut().enumerate().take(64).skip(1) {
        let info = e.player_info(i as i32);
        *slot = PlayerSlot {
            name: (!info.name_bytes.is_empty()).then_some(info.name_bytes),
            ping: info.ping as i16,
            packetloss: info.packetloss as u8,
            this_player: info.this_player,
            entity: e.entity(i as i32).is_some(),
        };
    }
    v
}

fn screen_size() -> (i32, i32) {
    let s = hud::screen();
    (s.width, s.height)
}

/// Runs the scoreboard's `f` with its inputs and the schemes.
fn with_board_inputs(f: impl FnOnce(&mut ScoreBoard, &scoreboard::Inputs, &vgui::Schemes)) {
    let players = player_slots();
    let (w, h) = screen_size();
    let (extra, teamplay, server, names) = hud::with(|h| (h.players.clone(), h.gamemode, h.server_name.clone(), team_names(h)));
    let inp = scoreboard::Inputs {
        players: &players,
        extra: &extra,
        teamplay,
        server_name: &server,
        team_names: &names,
        lookup: &hud::text::lookup,
        rules: crate::gameplay::hud_rules(hud::gameplay_id()),
        time: hud::now(),
        spectate_only: goldsrc::ext::spectator::is_spectate_only(),
        squelch: crate::voice::in_squelch_mode(),
        screen_h: h,
        screen_w: w,
    };
    vgui::with_schemes(|sb| with(|m| f(&mut m.board, &inp, sb)));
}

/// The viewport's team names: `TeamNames` stores at most four, localised (`0x10064e33`).
fn team_names(h: &hud::state::Hud) -> Vec<String> {
    h.team_names.iter().map(|n| localise(n)).collect()
}

/// The team menu's `Update` (`0x10066020`).
fn update_team_menu() {
    let players = player_slots();
    let (w, h) = screen_size();
    let (extra, names) = hud::with(|h| (h.players.clone(), team_names(h)));
    let frags: Vec<i16> = with(|m| m.board.teams.iter().map(|t| t.frags).collect());
    let team_frags = |i: usize| frags.get(i).copied().unwrap_or(0);
    // `pfnGetLevelName` is already `maps/<map>.bsp` (`Update` `0x10066020` uses it as it comes).
    let level = goldsrc::ext::render::level_name();
    let size = |t: &[u8]| vgui::with_schemes(|s| s.font_of("Briefing Text").map(|f| f.text_size(t))).flatten().unwrap_or((0, 0));
    let inp = team::Inputs {
        team_names: &names,
        players: &players,
        extra: &extra,
        team_frags: &team_frags,
        level: &level,
        load: &goldsrc::ext::render::load_file,
        briefing_size: &size,
        w,
        h,
    };
    with(|m| team_menu(m, w, h).update(&inp));
}

/// The viewport's team menu, made for the screen size and the ruleset's VGUI scale (a server of
/// the other ruleset rebuilds it).
fn team_menu(m: &mut Menus, w: i32, h: i32) -> &mut team::TeamMenu {
    let key = (w, h, Some(vgui::scale()));
    if m.team_size != key {
        m.team = None;
        m.team_size = key;
    }
    m.team.get_or_insert_with(|| team::TeamMenu::new(w, h, &hud::text::lookup))
}

/// The viewport's message handlers that feed its menus (`TeamNames` `0x10064df0`, `ScoreInfo`
/// `0x10065090`, `TeamScore` `0x10065120`, `TeamInfo` `0x100651c0`) and the death notice's
/// `gViewPort->DeathMsg` (`0x1001b360`). The HUD state has already taken the message.
pub fn on_message(m: Msg, buf: &[u8]) {
    use shared::protocol::hud as ph;
    match m {
        // `TeamNames` updates the team menu.
        Msg::TeamNames => update_team_menu(),
        // `UpdateOnPlayerInfo` (`0x10064750`): the team menu and the scoreboard.
        Msg::ScoreInfo => {
            update_team_menu();
            with_board_inputs(|b, inp, sb| b.update(inp, sb));
        }
        Msg::TeamInfo => with_board_inputs(|b, inp, sb| b.rebuild_teams(inp, sb)),
        Msg::TeamScore => {
            if let Some(t) = ph::TeamScore::decode(buf) {
                with(|mm| mm.board.team_score(&t.team, t.frags, t.deaths));
            }
        }
        Msg::DeathMsg => {
            if let Some(d) = ph::DeathMsg::decode(buf) {
                let players = player_slots();
                let me = (1..64).rev().find(|&i| players[i].this_player).map_or(0, |i| i as i32);
                let now = hud::now();
                with(|mm| {
                    if me != 0 {
                        mm.board.player_num = me;
                    }
                    mm.board.death_msg(d.killer as i32, d.victim as i32, now);
                });
            }
        }
        _ => {}
    }
}

// ---- VGUI menu queue -------------------------------------------------------------------------

/// `MsgFunc_VGUIMenu` (`0x10064f00`): menu 4 brings the map's name (63 characters at most) into
/// `m_sMapName`; then `ShowVGUIMenu` with whatever id came.
pub fn vgui_menu_message(id: i32, map_name: Option<Vec<u8>>) {
    if let Some(mut n) = map_name {
        n.truncate(63);
        with(|m| m.map_name = n);
    }
    show_vgui_menu(id);
}

/// "Briefing Text"'s `getTextSizeWrapped` (the message window sizes its text panel by it).
fn briefing_wrapped(text: &[u8], width: i32) -> (i32, i32) {
    vgui::with_schemes(|s| s.font_of("Briefing Text").map(|f| f.text_size_wrapped(text, width))).flatten().unwrap_or((0, 0))
}

/// `ShowVGUIMenu` (`0x10064050`): refused during demo playback and (but for the MOTD) at
/// intermission, and when the menu is already queued; the team menu only in teamplay
/// (`0x10064280`), the map briefing only when its file loads (`CreateTextWindow` returns null
/// otherwise). The first menu opens at once; later ones wait their turn.
pub fn show_vgui_menu(id: i32) {
    let e = engine();
    if e.demo_playing() || (hud::intermission() && id != 5) || with(|m| m.queue.iter().any(|q| q.id() == id)) {
        return;
    }
    let (w, h) = screen_size();
    let menu = match id {
        2 => {
            if hud::with(|h| h.gamemode) < 1 {
                return;
            }
            with(|m| team_menu(m, w, h).reset());
            Menu::Team
        }
        4 | 5 => {
            // The window shades the screen while the player has no class (`g_iPlayerClass`).
            let shade = crate::state::with(|s| s.player_class) == 0;
            let ok = hud::text::lookup("#Menu_OK");
            let window = if id == 5 {
                let (server, text) = hud::with(|h| (h.server_name.clone(), h.motd.clone()));
                motd::Motd::for_motd(&server, &text, &ok, shade, w, h, &briefing_wrapped)
            } else {
                // `pfnGetLevelName`: `maps/<map>.bsp` already.
                let level = goldsrc::ext::render::level_name();
                let level = (!level.is_empty()).then_some(level);
                let load = |p: &[u8]| goldsrc::ext::render::load_file(&hud::saytext::string(p));
                let mut name = with(|m| std::mem::take(&mut m.map_name));
                let made = motd::map_briefing(&mut name, level.as_deref().map(str::as_bytes), &load);
                with(|m| m.map_name = name);
                let Some((title, text)) = made else { return };
                motd::Motd::new(&title, &text, &ok, shade, w, h, &briefing_wrapped)
            };
            Menu::Text(id, Box::new(window), vgui::raise())
        }
        _ => return,
    };
    let first = with(|m| {
        m.queue.push(menu);
        m.queue.len() == 1
    });
    if first {
        open_top();
    }
}

/// `CMenuPanel::Open` for the shown menu: the team menu updates first (`0x10066610`).
fn open_top() {
    if with(|m| m.queue.first() == Some(&Menu::Team)) {
        update_team_menu();
    }
    crate::input::center_cursor();
}

/// `HideTopMenu` (`0x10064200`): close the shown menu and open the next (not during demo
/// playback, `0x10063bd0`).
fn hide_top_menu() {
    let next = with(|m| {
        if !m.queue.is_empty() {
            m.queue.remove(0);
        }
        !m.queue.is_empty()
    });
    if next && !engine().demo_playing() {
        open_top();
    }
}

fn current_menu() -> Option<i32> {
    with(|m| m.queue.first().map(Menu::id))
}

pub fn open_team_menu() {
    show_vgui_menu(2);
}

pub fn open_motd() {
    show_vgui_menu(5);
}

/// The team menu's digit keys through the `slot` commands: `SlotInput` of the shown menu.
pub fn team_menu_key(n: i32) -> bool {
    if current_menu() != Some(2) {
        return false;
    }
    team_slot(n);
    true
}

/// `CTeamMenuPanel::SlotInput` (`0x100665a0`), firing the button's action.
fn team_slot(n: i32) -> bool {
    let teams = hud::with(|h| h.team_names.len());
    let action = with(|m| m.team.as_ref().and_then(|t| t.slot_input(n, teams)));
    match action {
        Some(a) => {
            run_team_action(a);
            true
        }
        None => false,
    }
}

/// A team menu button's action: `CMenuHandler_StringCommandWatch` (3.0 `0x10064a20`, 2.1
/// `0x100b0bf0`) runs the command, closes the menu and, for `jointeam 1`..`jointeam 4`, sets
/// `g_iTeamNumber` ahead of the next player state (`0x10064a82..0x10064b5a`, issue #275); cancel
/// closes it.
fn run_team_action(a: team::Action) {
    let cmd = match a {
        team::Action::Command(c) => {
            engine().client_cmd(&c);
            Some(c)
        }
        team::Action::Close => None,
    };
    hide_top_menu();
    if let Some(n) = cmd.as_deref().and_then(local_team_of) {
        crate::state::with(|s| s.team_number = n);
    }
}

/// The team menu's answer without the menu: a client extension's `menu_reply` to a team menu it
/// took over (port addition, issue #314). What [`run_team_action`] does for a team button, with
/// no menu of the port's to close.
pub fn join_team(n: i32) {
    let cmd = format!("jointeam {n}");
    engine().client_cmd(&cmd);
    if let Some(t) = local_team_of(&cmd) {
        crate::state::with(|s| s.team_number = t);
    }
}

/// The team `CMenuHandler_StringCommandWatch` stores for its command: TS compares the whole
/// string with `"jointeam 1"`..`"jointeam 4"` (`strcmp`), so auto-assign (`jointeam 5`) sets none.
fn local_team_of(cmd: &str) -> Option<i32> {
    (1..=4).find(|n| cmd == format!("jointeam {n}"))
}

// ---- scoreboard ------------------------------------------------------------------------------

/// `ShowScoreBoard` (`0x10063710`, multiplayer only) opens it (`ScorePanel::Open` `0x1005e630`:
/// rebuild the teams); `HideScoreBoard` (`0x10063760`, not at intermission) hides it and ends
/// squelch mode.
pub fn show_scores(on: bool) {
    if on {
        if engine().max_clients() > 1 {
            open_scores();
        }
    } else if !hud::intermission() {
        with(|m| m.scores = false);
        crate::voice::stop_squelch_mode();
    }
}

fn open_scores() {
    with_board_inputs(|b, inp, sb| b.rebuild_teams(inp, sb));
    with(|m| m.scores = true);
}

/// A mouse press while the scoreboard is up and has no cursor (`ScorePanel::mousePressed`
/// 1005e660, through its hit-test panel): the first press enters squelch mode, which shows the
/// cursor. Returns true if the press was taken (it doesn't reach the engine as a key).
pub fn scoreboard_press() -> bool {
    if !scores_visible() || wants_cursor() || hud::intermission() {
        return false;
    }
    crate::voice::start_squelch_mode();
    crate::input::center_cursor();
    true
}

pub fn scores_visible() -> bool {
    with(|m| m.scores)
}

/// Intermission starts or ends (`CHud::Redraw` `0x10021aa0`): TS hides the VGUI menus
/// (`HideAllVGUIMenus` `0x100641e0`) and shows the scoreboard; leaving hides it.
pub fn intermission(on: bool) {
    if on {
        with(|m| {
            m.buy.close();
            m.queue.clear();
        });
    }
    show_scores(on);
}

/// `TeamFortressViewport::AllowedToPrintText` (`0x10064240`): false while the team menu or the
/// MOTD (VGUI menus 2 and 5; TS also lists the class and class-help menus 3 and 6, which the
/// port doesn't have) is shown and the local player has no class (`g_iPlayerClass`, `0x10190a10`).
/// Chat then goes to the console only.
pub fn allowed_to_print_text() -> bool {
    if !matches!(current_menu(), Some(2 | 5)) {
        return true;
    }
    crate::state::with(|s| s.player_class) != 0
}

// ---- input -----------------------------------------------------------------------------------

/// A menu owns the mouse (virtual cursor, clicks): `UpdateCursorState` (`0x10064790`).
pub fn wants_cursor() -> bool {
    with(|m| m.buy.active || !m.queue.is_empty()) || squelching()
}

/// Squelch mode with the scoreboard showing: the scoreboard owns the cursor.
fn squelching() -> bool {
    crate::voice::in_squelch_mode() && scores_visible()
}

const K_ENTER: i32 = 13;
const K_ESCAPE: i32 = 27;
const K_SPACE: i32 = 32;
/// GoldSrc keydefs.h `K_KP_ENTER` (169, `0xa9` in `0x10064b90`).
const K_KP_ENTER: i32 = 169;

/// `TeamFortressViewport::KeyInput` (`0x10064b90`, from `HUD_Key_Event`), with the console up:
/// * Enter (or keypad Enter) while spectating asks for the team menu;
/// * with the team menu shown, Escape closes it once the player has a team (`g_iTeamNumber`,
///   `0x10190b24`), a digit goes to its `SlotInput` and `+jump`
///   auto-assigns;
/// * with the map briefing or the MOTD shown (menus 4 and 5; TS's list also has the class help,
///   6), Enter, keypad Enter, Space or Escape close it.
///
/// The buy menu takes no keys here (Escape doesn't close it). Returns true if a menu consumed
/// the key.
pub fn key_event(down: bool, key: i32, binding: &CStr) -> bool {
    let e = engine();
    if down && matches!(key, K_ENTER | K_KP_ENTER) && crate::state::with(|s| s.spec_mode) != 0 && !e.con_is_visible() {
        show_vgui_menu(2);
    }
    let Some(cur) = current_menu() else { return false };
    if e.con_is_visible() {
        return false;
    }
    if cur == 2 {
        if key == K_ESCAPE && crate::state::with(|s| s.team_number) != 0 {
            hide_top_menu();
            return true;
        }
        if down && (b'0' as i32..=b'9' as i32).contains(&key) {
            team_slot(key - b'0' as i32);
            return true;
        }
    }
    if down && matches!(key, K_ENTER | K_KP_ENTER | K_SPACE | K_ESCAPE) && matches!(cur, 4 | 5) {
        hide_top_menu();
        return true;
    }
    if down && cur == 2 && binding.to_bytes() == b"+jump" {
        team_slot(5);
        return true;
    }
    false
}

/// Mouse buttons while a menu has the cursor. VGUI's buttons fire on a left release over them;
/// the buy menu gets the buttons only while no VGUI menu is up (`IN_MouseEvent` `0x100273d0`):
/// `CTSBuyMenu::MouseEvent` (10038360) runs the row under the cursor on a left release (key 1 of
/// the current page when there is none) and key 10 (back) on a right release.
pub fn mouse_buttons(state: i32, old: i32, x: i32, y: i32) {
    let pressed = state & !old;
    let released = old & !state;
    let (w, h) = screen_size();
    match current_menu() {
        Some(4 | 5) => {
            let close = with(|m| match m.queue.first_mut() {
                Some(Menu::Text(_, t, _)) => {
                    if pressed != 0 {
                        t.press(w, h, x, y);
                    }
                    released != 0 && t.release(w, h, x, y, released & 1 != 0)
                }
                _ => false,
            });
            if close {
                hide_top_menu();
            }
            return;
        }
        Some(_) => {
            let action = with(|m| {
                let t = m.team.as_mut()?;
                if pressed != 0 {
                    t.press(w, h, x, y);
                }
                if released != 0 { t.release(w, h, x, y, released & 1 != 0) } else { None }
            });
            if let Some(a) = action {
                run_team_action(a);
            }
            return;
        }
        None => {}
    }
    if squelching() && !buy_open() {
        // The list's scroll bar takes presses and releases on it (the hit-test panel is hidden
        // in squelch mode, so they reach it).
        let (sw, sh) = screen_size();
        let taken = with(|m| {
            let mut t = false;
            if pressed != 0 {
                t |= m.board.scroll_pressed(sw, sh, x, y);
            }
            if released != 0 {
                t |= m.board.scroll_released(sw, sh, x, y, released & 1 != 0);
            }
            t
        });
        if taken {
            return;
        }
        // `ScorePanel::mousePressed` from a player-entry label: any button toggles the
        // highlighted player's mute.
        if pressed != 0
            && !hud::intermission()
            && let Some(p) = with(|m| m.board.on_entry(x, y).then(|| m.board.highlighted_player()).flatten())
        {
            crate::voice::scoreboard_toggle(p);
        }
        return;
    }
    if released & 1 != 0 {
        click(x, y);
    } else if released & 2 != 0 && buy_open() {
        buy_key(10);
    }
}

fn click(x: i32, y: i32) {
    if !buy_open() {
        return;
    }
    let r = rules();
    let h = hud::screen().height;
    let (hit, actions) = with(|m| {
        let hit = m.buy.hit_test(x, y, h, &r);
        let actions = match hit {
            buy::Hit::Row(pg, key) => m.buy.handle_key(pg, key, &r, &mut EngineRng),
            buy::Hit::Miss => {
                let pg = m.buy.page;
                m.buy.handle_key(pg, 1, &r, &mut EngineRng)
            }
            buy::Hit::Unset => Vec::new(),
        };
        (hit, actions)
    });
    if let (buy::Hit::Unset, Some(msg)) = (hit, r.menu.hole_message) {
        engine().center_print(msg);
    }
    run_actions(actions);
}

// ---- drawing ---------------------------------------------------------------------------------

/// A TS menu column (`FUN_1004fed0` / `FUN_1004ff10`): rows of 128×20, a translucent box per row.
/// `FUN_1004ff10` takes a brightness scale, 1.0 or 0.5 (`dim`): the box is the row colour at
/// alpha `ftol(50·scale)` and the text the row colour times the scale.
struct Column<'a> {
    dc: &'a Dc<'a>,
    r: &'a Rules,
    x: i32,
    y: i32,
    page: i32,
    dim: bool,
    rows: &'a mut Vec<(i32, i32, i32, i32, i32, i32)>,
}

impl Column<'_> {
    fn row(&mut self, text: &str, rgb: [i32; 3], key: i32) {
        let dim = self.dim;
        self.row_scaled(text, rgb, key, dim);
    }

    /// A row with its own brightness: TS draws the chosen category and weapon at full
    /// brightness in a dimmed column.
    fn row_scaled(&mut self, text: &str, rgb: [i32; 3], key: i32, dim: bool) {
        let (w, h) = (128, 20);
        let scale = if dim { 0.5 } else { 1.0 };
        self.dc.fill(self.x, self.y, w - 1, h - 1, rgb, hud::draw::ftol(50.0 * scale));
        let t = rgb.map(|c| hud::draw::ftol(c as f64 * scale));
        self.dc.string(self.x, self.y, &buy_text(self.r, text), t);
        if key > 0 {
            self.rows.push((self.x, self.y, w, h, self.page, key));
        }
        self.y += h;
    }
}

const RED: [i32; 3] = [195, 45, 45];
const GREEN: [i32; 3] = [45, 195, 45];
const BACK: [i32; 3] = [100, 100, 205];

fn draw_buy(dc: &Dc, b: &BuyMenu, r: &Rules, rows: &mut Vec<(i32, i32, i32, i32, i32, i32)>) {
    let hud2 = half(dc.hud);
    let y0 = dc.h() / 2;
    let fmt = hud::text::format;
    use hud::text::Arg::I;
    if b.page == page::TEAM {
        let mut c = Column { dc, r, x: 8, y: y0, page: page::TEAM, dim: false, rows };
        c.row("#Chg_ChangeTeam", GREEN, 0);
        c.row("1 #Chg_Join Specialists", hud2, 1);
        c.row("2 #Chg_Join Mercenaries", hud2, 2);
        c.row("0 #Chg_Cancel", BACK, 10);
        return;
    }
    draw_summary(dc, b, r);
    let cur = &b.current;
    // Wrapping: a loaded `tsbuylist.tsa` config's totals can be anything (C int arithmetic).
    let affordable = |cost: i32, slots: i32| cost.wrapping_add(cur.total) <= r.cash && slots.wrapping_add(cur.slots) <= buy::SLOTS;

    // Columns are drawn child-first, parents after (each TS page function calls its parent).
    if b.page == page::OPTIONS {
        let id = b.pending_weapon;
        let base = buy::cost(id, b.pending_att, false, false);
        let mut c = Column { dc, r, x: 0x188, y: y0, page: page::OPTIONS, dim: false, rows };
        c.row("1 #Buy_Buy", if affordable(base, buy::slots_for(id, false, false, r)) { GREEN } else { RED }, 1);
        let labels = ["#HUD_Silencer", "#HUD_Lasersight", "#HUD_Flashlight", "#HUD_Scope"];
        let prices = [900, 800, 500, 1800];
        for bit in 0..4 {
            if b.attachment_allowed(1 << bit) {
                // The label shows the attachment's own price; the colour tests base + attachment.
                let price = base.wrapping_add(prices[bit]);
                let text = fmt(&format!("%i {}  %i", labels[bit]), &[I(bit as i64 + 2), I(prices[bit] as i64)]);
                c.row(&text, if price.wrapping_add(cur.total) <= r.cash { hud2 } else { RED }, bit as i32 + 2);
            }
        }
        if let Some(w) = shared::weapons::tables().weapon(id.max(0) as u8) {
            if w.akimbo != -1 {
                let ok = affordable(buy::cost(id, b.pending_att, true, false), w.slots * 2);
                c.row("6 #Buy_Akimbo", if ok { GREEN } else { RED }, 6);
            } else if w.stack {
                let ok = affordable(buy::cost(id, 0, true, false), w.slots * w.pellets);
                c.row("6 #Buy_FullLoad", if ok { GREEN } else { RED }, 6);
            }
        }
        c.row("0 #Buy_GoBack", BACK, 10);
    }
    if matches!(b.page, page::WEAPONS | page::OPTIONS) {
        let mut c = Column { dc, r, x: 0x108, y: y0, page: page::WEAPONS, dim: b.page != page::WEAPONS, rows };
        for row in 0..12 {
            let id = buy::table(&r.menu)[row][b.category.clamp(0, 4) as usize];
            if id == -1 || buy::restricted(id, false, r) {
                continue;
            }
            let Some(w) = shared::weapons::tables().weapon(id as u8) else { continue };
            let (key, ch) = if row < 9 { (row as i32 + 1, (b'1' + row as u8) as char) } else { (11, 'A') };
            let ok = affordable(w.cost, w.slots);
            // `0x10037ad4`: full brightness on the weapon page and for the chosen weapon.
            let dim = b.page != page::WEAPONS && id != b.pending_weapon;
            c.row_scaled(&format!("{ch} {}", w.name), if ok { hud2 } else { RED }, key, dim);
            // `0x10037b44..0x10037b57`: the box is drawn from the chosen weapon's row, so a
            // weapon that isn't listed (restricted since it was chosen) has none.
            if id == b.pending_weapon {
                draw_weapon_box(dc, r, id);
            }
        }
        if b.page == page::WEAPONS {
            c.row("0 #Buy_GoBack", BACK, 10);
        }
    }
    if b.page == page::ITEMS {
        let mut c = Column { dc, r, x: 0x108, y: y0, page: page::ITEMS, dim: false, rows };
        for (key, id, label, price) in [(1i32, buy::ITEM_KEVLAR, "Kevlar", 1900i32), (2, buy::ITEM_STEALTH, "Stealth Shoes", 700)] {
            if buy::restricted(id, true, r) {
                continue;
            }
            let slots = buy::slots_for(id, false, true, r);
            let text = fmt(&format!("%i {label}  %i"), &[I(key as i64), I(price as i64)]);
            let ok = if r.menu.items_red_by_cash_only { price.wrapping_add(cur.total) <= r.cash } else { affordable(price, slots) };
            c.row(&text, if ok { hud2 } else { RED }, key);
        }
        c.row("0 #Buy_GoBack", BACK, 10);
    }
    if matches!(b.page, page::CATEGORY | page::WEAPONS | page::OPTIONS | page::ITEMS) {
        let mut c = Column { dc, r, x: 0x88, y: y0, page: page::CATEGORY, dim: b.page != page::CATEGORY, rows };
        for (i, name) in buy::CATEGORIES.iter().enumerate() {
            // `0x1003788c`: the chosen category keeps full brightness.
            let dim = b.page != page::CATEGORY && i as i32 != b.category;
            c.row_scaled(&format!("{} {name}", i + 1), hud2, i as i32 + 1, dim);
        }
        if !r.menu.items_need_teamplay || r.gamemode > 1 {
            c.row("6 #Buy_PlayerAccessory", hud2, 6);
        }
        if b.page == page::CATEGORY {
            c.row("0 #Buy_GoBack", BACK, 10);
        }
    }
    if b.page == page::REMOVE {
        let mut c = Column { dc, r, x: 0x88, y: y0, page: page::REMOVE, dim: false, rows };
        for (i, e) in cur.entries.iter().enumerate().filter(|(_, e)| e.id != 0) {
            c.row(&format!("{} {}", i + 1, BuyMenu::entry_name(e)), hud2, i as i32 + 1);
        }
        c.row("0 #Buy_GoBack", BACK, 10);
    }
    if matches!(b.page, page::SAVE | page::LOAD) {
        let save = b.page == page::SAVE;
        let mut c = Column { dc, r, x: 0x88, y: y0, page: b.page, dim: false, rows };
        c.row(if save { "#Buy_SaveEquipment" } else { "#Buy_LoadEquipment" }, GREEN, 0);
        for (i, s) in b.saved.iter().enumerate() {
            let key = if s.total < 1 { "#Buy_SlotEmpty" } else { "#Buy_SlotAvailable" };
            let text = fmt(&buy_text(r, key), &[I(i as i64 + 1)]);
            let red = !save && s.total > r.cash;
            c.row(&text, if red { RED } else { hud2 }, i as i32 + 1);
        }
        c.row("0 #Buy_GoBack", BACK, 10);
    }
    // Main page.
    let mut c = Column { dc, r, x: 8, y: y0, page: page::MAIN, dim: b.page != page::MAIN, rows };
    if cur.total < 1 {
        c.row("1 #Buy_EmptyConfig", RED, 1);
    } else {
        let t = if b.new_config { "1 #Buy_UseNewConfig" } else { "1 #Buy_UseLastConfig" };
        c.row(t, if r.cash.wrapping_sub(cur.total) < 0 { RED } else { GREEN }, 1);
    }
    c.row("2 #Buy_Buy", hud2, 2);
    c.row("3 #Buy_RemoveItem", hud2, 3);
    c.row("4 #Buy_ResetConfig", hud2, 4);
    c.row("5 #Buy_SaveConfig", hud2, 5);
    c.row("6 #Buy_LoadConfig", hud2, 6);
    c.row("7 #Buy_Random", GREEN, 7);
    if b.page == page::MAIN {
        c.row("0 #Buy_Cancel", BACK, 10);
    }
}

/// The buy menu's version box (port addition): the server's ruleset, this client library's
/// version and the server library's (`SrvSett` 5..7; TS 3.0's server sends none).
fn version_lines(ruleset: &str, server: Option<[i16; 3]>) -> [String; 3] {
    let server = match server {
        Some([a, b, c]) => format!("Server {a}.{b}.{c}"),
        None => "Server version unknown".to_owned(),
    };
    [ruleset.to_owned(), format!("Client {}", shared::protocol::port::VERSION), server]
}

/// The right edge of the buy menu's columns: the options column (`0x188`, 128 wide) and the
/// weapon box (`0x108`, `0xff` wide). The version box stays right of it.
const COLUMNS_RIGHT: i32 = 0x188 + 128;

/// Draws [`version_lines`] in the bottom right corner as rows like the menu's own (a translucent
/// box of the row colour, dimmed, with the text), clear of the columns and the summary.
fn draw_versions(dc: &Dc, lines: &[String]) {
    let rgb = half(dc.hud);
    let text_w = lines.iter().map(|l| dc.string_len(l)).max().unwrap_or(0);
    let (x, mut y, w) = version_box(dc.w(), dc.h(), text_w, lines.len() as i32);
    for line in lines {
        dc.fill(x, y, w - 1, VERSION_ROW - 1, rgb, hud::draw::ftol(50.0 * 0.5));
        dc.string(x + 4, y, line, rgb);
        y += VERSION_ROW;
    }
}

/// The version box's rows: the menu's row height.
const VERSION_ROW: i32 = 20;

/// The version box's left, top and width for `rows` lines of up to `text_w` pixels on a
/// `screen_w`×`screen_h` screen: 8 pixels from the right edge and above the spectator bar (the
/// buy menu is open only while dead or observing, under the bar), never left of the columns
/// ([`COLUMNS_RIGHT`]; on a narrow screen the text is cut at the screen's edge instead).
fn version_box(screen_w: i32, screen_h: i32, text_w: i32, rows: i32) -> (i32, i32, i32) {
    let w = text_w + 8;
    let x = (screen_w - 8 - w).max(COLUMNS_RIGHT + 8);
    let bottom = screen_h - crate::spectator::panel_height(screen_h) - 8;
    (x, bottom - VERSION_ROW * rows, w)
}

/// `FUN_10038180`: sprite, price and slots of the highlighted weapon.
fn draw_weapon_box(dc: &Dc, r: &Rules, id: i32) {
    let Some(w) = shared::weapons::tables().weapon(id as u8) else { return };
    let (x, y) = (0x108, dc.h() / 2 - 0x30);
    dc.fill(x, y, 0xff, 0x2f, dc.hud, 0x19);
    if let Some(&s) = dc.g.ts.weapons.get(id as usize) {
        dc.spr(s, x, y, dc.hud);
    }
    let f = hud::text::format;
    dc.string(x + 128, y, &f(&buy_text(r, "#Buy_Credits"), &[hud::text::Arg::I(w.cost as i64)]), half(dc.hud));
    dc.string(x + 128, y + 20, &f(&buy_text(r, "#Buy_Slots"), &[hud::text::Arg::I(w.slots as i64)]), half(dc.hud));
}

/// `FUN_10036af0`: totals bar and the loadout per bucket with sprites, ammo and attachments.
fn draw_summary(dc: &Dc, b: &BuyMenu, r: &Rules) {
    let hud2 = half(dc.hud);
    let cur = &b.current;
    use hud::text::Arg::I;
    dc.fill(8, 8, 0x280, 0x13, hud2, 0x32);
    let (text, args) = buy::summary_header(b, r);
    let total = hud::text::format(&buy_text(r, text), &args.iter().map(|&a| I(a as i64)).collect::<Vec<_>>());
    dc.string(8, 8, &total, if r.cash.wrapping_sub(cur.total) <= 0 { RED } else { hud2 });
    let free_slots = buy::SLOTS.wrapping_sub(cur.slots);
    let free = hud::text::format(&buy_text(r, "#Buy_FreeSlots"), &[I(free_slots as i64)]);
    dc.string(0x108, 8, &free, if free_slots <= 0 { RED } else { hud2 });
    let t = shared::weapons::tables();
    for bucket in 1..=5 {
        let x = bucket * 0x80 - 0x78;
        dc.fill(x, 0x1c, 0x80, 0x14, hud2, 0x32);
        dc.number(x, 0x1c, hud::draw::num::DIGITS_2, bucket, hud2);
        let mut y = 0x30;
        for e in cur.entries.iter().filter(|e| e.id != 0) {
            if e.is_item {
                if bucket != 1 {
                    continue;
                }
                dc.string(x, y + 5, &BuyMenu::entry_name(e), buy::summary_item_rgb(e.id, r, dc.hud));
                y += 0x30;
                continue;
            }
            // `FUN_10036af0`: the gun in its own bucket, then for an akimbo entry the akimbo
            // weapon in its bucket too, always with the gun's ammo (flags 5) and the same
            // attachments.
            let Some(base) = t.weapon(e.id as u8) else { continue };
            let rounds = cur.ammo[base.ammo as usize & 15].0;
            let akimbo = (e.akimbo && base.akimbo >= 0).then_some(base.akimbo);
            for (id, second) in std::iter::once((e.id, false)).chain(akimbo.map(|a| (a, true))) {
                let Some(w) = t.weapon(id as u8) else { continue };
                if w.hud[0] as i32 != bucket {
                    continue;
                }
                let rgb = if buy::restricted(id, false, r) { RED } else { dc.hud };
                if let Some(&s) = dc.g.ts.weapons.get(id as usize) {
                    dc.spr(s, x, y, rgb);
                }
                if second {
                    dc.number(x, y + 5, hud::draw::num::DIGITS_3 | hud::draw::num::DRAW_ZERO, rounds, dc.hud);
                } else if e.count != 0 {
                    dc.number(x, y + 5, hud::draw::num::DIGITS_2, e.count, dc.hud);
                } else if w.ammo != 0 {
                    dc.number(x, y + 5, hud::draw::num::DIGITS_3 | hud::draw::num::DRAW_ZERO, rounds, dc.hud);
                }
                let s = buy::attachment_summary(e.att, &|k| buy_text(r, k));
                if !s.is_empty() {
                    dc.string(x, y + 0x14, &s, hud2);
                }
                y += 0x30;
            }
        }
    }
}

/// The viewport's per-frame work (`paintBackground` `0x100648c0`) and the VGUI paint pass: the
/// scoreboard follows the cursor and updates every half second while shown; the shown menu
/// follows the cursor; then the team menu, the scoreboard and the MOTD paint in the viewport's
/// child order.
fn paint_vgui() {
    let (w, h) = screen_size();
    let (cx, cy) = crate::input::cursor();
    let now = hud::now();
    let squelch = crate::voice::in_squelch_mode();
    let audible = |p: i32| {
        let info = engine().player_info(p);
        let blocked_self = info.this_player && !goldsrc::ext::spectator::is_spectate_only();
        (!info.name_bytes.is_empty(), blocked_self, crate::voice::is_player_audible(p))
    };
    let shown = scores_visible();
    if shown {
        with(|m| m.board.scroll_cursor(w, h, cx, cy));
        with(|m| m.board.cursor_moved(cx, cy, squelch, &audible));
        if with(|m| m.board.next_update < now) {
            with_board_inputs(|b, inp, sb| b.update(inp, sb));
            with(|m| m.board.next_update = now + 0.5);
        }
    }
    let cur = with(|m| m.queue.first().map(Menu::id));
    // `0x10064906`: a new local team number (`g_iTeamNumber`) updates the shown menu, which
    // only the team menu acts on (issue #268).
    let team = crate::state::with(|s| s.team_number);
    if with(|m| m.note_team(team)) && cur == Some(2) {
        update_team_menu();
    }
    if wants_cursor() {
        with(|m| match m.queue.first_mut() {
            Some(Menu::Text(_, t, _)) => t.hover(w, h, cx, cy),
            Some(Menu::Team) => {
                if let Some(t) = &mut m.team {
                    t.hover(w, h, cx, cy);
                }
            }
            None => {}
        });
    }
    if cur == Some(2) && with(|m| m.team.as_ref().is_some_and(|t| !t.updated_map)) {
        update_team_menu();
    }
    let arrows = scroll::ctf_arrows(w, &|p| vgui::image(p, true));
    let list_arrows = [vgui::image("gfx/vgui/arrowup.tga", true), vgui::image("gfx/vgui/arrowdown.tga", true)];
    vgui::with_schemes(|sb| {
        let s = &mut EngineSurface;
        with(|m| {
            if cur == Some(2)
                && let Some(t) = &m.team
            {
                t.paint(s, sb, w, h, &arrows);
            }
        });
        if shown {
            let mut voice = |p: i32| crate::voice::scoreboard_icon(p);
            // Port addition: a client extension may hide it (issue #314).
            hud::elements::draw(ts_client_ext_api::hud::SCOREBOARD, || with(|m| m.board.paint(s, sb, w, h, &mut voice, &list_arrows)));
        }
    });
    // Then the panels parented since: the voice labels and talk icon, and the message window,
    // in the order they were parented (VGUI1 paints children in list order).
    let text_z = with(|m| match m.queue.first() {
        Some(Menu::Text(_, _, z)) => Some(*z),
        _ => None,
    });
    crate::voice::redraw(shown, |z| text_z.is_none_or(|t| z < t));
    if let Some(t) = text_z {
        vgui::with_schemes(|sb| {
            with(|m| {
                if let Some(Menu::Text(_, window, _)) = m.queue.first() {
                    window.paint(&mut EngineSurface, sb, w, h, &arrows);
                }
            })
        });
        crate::voice::redraw(shown, |z| z > t);
    }
}

/// The buy menu's part of `CHudTSWeapons::Draw` (`0x1004d3f0`), which runs only with `nohud` 0
/// and, like every element, not under `HIDEHUD_ALL` or in intermission: the menu is drawn
/// (right after the laser dots, under the rest of the weapons element), then closed once you're
/// alive again (except the team page). Hidden, it stays open and keeps its keys and cursor.
pub fn draw_buy_menu(dc: &Dc) {
    let r = rules();
    let active = with(|m| {
        m.rows.clear();
        if m.buy.active {
            draw_buy(dc, &m.buy, &r, &mut m.rows);
        }
        m.buy.active
    });
    if active {
        let (gameplay, server) = hud::with(|h| (h.gameplay, h.server_version));
        draw_versions(dc, &version_lines(crate::gameplay::hud_rules(gameplay).ruleset_name(), server));
    }
    if buy_open() && !hud::dead_or_observer() && with(|m| m.buy.page != page::TEAM) {
        with(|m| m.buy.close());
    }
}

pub fn redraw(time: f32) {
    let _ = time;
    paint_vgui();
    if !scores_visible() {
        // Squelch mode ends with the scoreboard (also when intermission hid it).
        crate::voice::stop_squelch_mode();
    }
    hud::with_dc(|dc| {
        if wants_cursor() {
            let (cx, cy) = crate::input::cursor();
            if dc.g.ts.cursor != 0 {
                dc.spr(dc.g.ts.cursor, cx - 16, cy - 16, dc.hud);
            } else {
                dc.fill(cx - 1, cy - 1, 3, 3, [255; 3], 255);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CMenuHandler_StringCommandWatch` (`0x10064a82..0x10064b5a`, 2.1 `0x100b0bf0`): only the
    /// exact `jointeam 1`..`4` strings set `g_iTeamNumber` (issue #275).
    #[test]
    fn jointeam_sets_the_local_team() {
        assert_eq!(
            (1..=5).map(|n| local_team_of(&format!("jointeam {n}"))).collect::<Vec<_>>(),
            [Some(1), Some(2), Some(3), Some(4), None]
        );
        assert_eq!(local_team_of("jointeam 1 "), None);
        assert_eq!(local_team_of("jointeam 01"), None);
    }

    /// `FUN_10036480`: a missing `tsbuylist.tsa` is created empty, an existing one is read, and
    /// when it can't be created either the path is dropped so nothing is saved.
    #[test]
    fn read_buylist_reads_creates_or_forgets() {
        let dir = std::env::temp_dir().join(format!("ts-buylist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tsbuylist.tsa");
        let path_s = path.to_str().unwrap();
        let _ = std::fs::remove_file(&path);
        let (saved, p) = read_buylist(path_s);
        assert_eq!(p, path_s);
        assert!(saved.iter().all(|c| *c == buy::Config::default()));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0, "created empty (\"wb\")");

        let mut configs: [buy::Config; buy::SAVED_CONFIGS] = Default::default();
        configs[4].total = 4200;
        std::fs::write(&path, buy::write_buylist(&configs)).unwrap();
        assert_eq!(read_buylist(path_s), (configs, path_s.to_owned()));

        let missing = dir.join("no-such-dir").join("tsbuylist.tsa");
        let (saved, p) = read_buylist(missing.to_str().unwrap());
        assert_eq!(p, "", "neither \"rb\" nor \"wb\" opened it");
        assert!(saved.iter().all(|c| *c == buy::Config::default()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Issue #268: a change of the local team number is seen once (TS then updates the shown
    /// menu); the same number again isn't a change.
    #[test]
    fn team_number_changes() {
        let mut m = Menus::default();
        assert!(!m.note_team(0), "starts at 0");
        assert!(m.note_team(2));
        assert!(!m.note_team(2));
        assert!(m.note_team(1) && m.team_number == 1);
    }

    #[test]
    fn version_lines_name_the_ruleset_and_both_libraries() {
        let v = shared::protocol::port::VERSION;
        assert_eq!(version_lines("TS 3.0", Some([3, 0, 2])), ["TS 3.0".to_owned(), format!("Client {v}"), "Server 3.0.2".to_owned()]);
        assert_eq!(version_lines("TS 3.0", None)[2], "Server version unknown", "TS 3.0's server");
    }

    /// The box sits in the bottom right corner above the spectator bar, right of every column
    /// and below the summary and the options column.
    #[test]
    fn version_box_clears_the_menu() {
        for (sw, sh) in [(640, 480), (800, 600), (940, 924), (1920, 1080)] {
            let (x, y, w) = version_box(sw, sh, 120, 3);
            assert!(x > COLUMNS_RIGHT, "{sw}x{sh}: right of the columns");
            let bar = crate::spectator::panel_height(sh);
            assert_eq!(y + 3 * VERSION_ROW, sh - bar - 8, "{sw}x{sh}: just above the spectator bar");
            assert!(y > 0x30, "{sw}x{sh}: below the summary");
            assert!(sw < 700 || x + w == sw - 8, "{sw}x{sh}: in the corner");
        }
    }
}
