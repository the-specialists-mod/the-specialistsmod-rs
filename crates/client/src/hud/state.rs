//! HUD state and the message handlers that write it (TS `CHudTSStatus` 0x100d43cc,
//! `CHudTSWeapons` 0x100d0ca0, the viewport's `g_PlayerExtraInfo`, and the SDK elements TS
//! kept). Handlers are pure: engine side effects come back as [`Effect`]s so they can be tested.

use super::text::{Titles, localise_with, lookup_string};
use crate::gameplay::{ClientRules, KungfuRegen, StuntCooldown};
use goldsrc::ext::hud::Screen;
use shared::protocol::Msg;
use shared::protocol::hud as p;
use shared::protocol::port;
use shared::weapons::MAX_WEAPONS;

pub const MAX_CLIENTS: usize = 64;
/// Weapon grid: 17 rows × 11 buckets (`m_rgSlots` at `+0x164`, index `bucket + row*11`).
pub const GRID_ROWS: usize = 17;
pub const GRID_BUCKETS: usize = 11;
pub const MAX_TS_MESSAGES: usize = 16;
pub const MAX_OBJECTIVES: usize = 32;
/// The team names `TeamNames` keeps (`0x10064df0` stores teams 1..4).
pub const MAX_TEAM_NAMES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    /// TS weapon id, or -1 when empty.
    pub id: i32,
    pub clip: i32,
}
pub const EMPTY: Slot = Slot { id: -1, clip: -1 };

/// One timed TS message (`0x9c`-byte records at `+0x70`).
#[derive(Debug, Clone, Default)]
pub struct TsLine {
    pub expire: f32,
    pub rgb: [i32; 3],
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub flags: u8,
}

#[derive(Debug, Clone, Default)]
pub struct Objective {
    pub visible: bool,
    pub pos: [f32; 3],
    pub label: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FlagState {
    pub state: u8,
    pub carrier: u8,
    pub team: u8,
}

/// `g_PlayerExtraInfo` / `g_PlayerItems` per client.
#[derive(Debug, Clone, Default)]
pub struct PlayerExtra {
    pub frags: i32,
    pub deaths: i32,
    pub class: i32,
    pub team: i32,
    pub teamname: String,
    pub award: u8,
    pub observer: bool,
    pub active_items: u8,
    pub laser_color: u8,
}

#[derive(Debug, Clone, Default)]
pub struct TeamScore {
    pub name: String,
    pub frags: i32,
    pub deaths: i32,
}

/// `CHudMenu` (SDK menu.cpp): the `ShowMenu` text menu.
#[derive(Debug, Clone, Default)]
pub struct TextMenu {
    pub displayed: bool,
    pub text: String,
    /// `g_szPrelocalisedMenuString` (512 bytes).
    pub pending: String,
    /// `m_fWaitingForMore`: the last chunk said more follows.
    pub waiting: bool,
    pub slots: u16,
    /// Absolute time to hide, 0 = until a selection.
    pub shutoff: f32,
}

/// Engine-side effects the caller applies after a handler.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Sound(&'static str),
    /// `goslow.wav` as `MsgFunc_TSSlowMo` plays it (`0x1004bf53..0x1004bf75`):
    /// `EV_PlaySound(-1, gHUD.m_vecOrigin, 0, "goslow.wav", 1.0, 0.8, 0, 100)`.
    GoSlow,
    /// Run the `thirdperson` toggle (`0x1004fe00`) now, as TS's handlers do: the death cam
    /// (`TSHealth`), `cl_thirdpersonswitch` (`WeaponInfo`) and `Spectator`.
    ToggleThirdPerson,
    OpenBuyMenu,
    /// `MsgFunc_VGUIMenu` (`0x10064f00`): the menu id and, for menu 4, the map name.
    VguiMenu(i32, Option<Vec<u8>>),
    OpenMotd,
    /// `Spectator` for the local player (`0x100652fd` -> `0x1000b9d0`): clear the predicted spread
    /// penalty ([`crate::prediction::reset_spread`]).
    ResetSpread,
    /// The server runs another gameplay ruleset than the last one the client saw (`SrvSett` 4):
    /// say so on the console.
    GameplayRuleset(i16),
    /// The server's port version is complete (`SrvSett` 7, after 5 and 6; port addition): compare
    /// it with this client's (`super::version`).
    ServerVersion([i16; 3]),
    /// Forward to the effects package (`crate::state::Shared::ts_effects`).
    Fx(crate::state::TsEffect),
    Fade(p::TsFade),
    ConsolePrint(String),
    CenterPrint(String),
    /// `CHudSayText::SayTextPrint(text, client)` ([`super::say_text_print`]).
    SayText(String, i32),
}

/// Context for handlers: what TS reads from globals while parsing.
pub struct Ctx<'a> {
    pub now: f32,
    pub screen: &'a Screen,
    pub titles: &'a dyn Titles,
    pub local_index: i32,
    pub local_name: String,
    pub third_person: bool,
    /// `cl_thirdpersonswitch == 1` (`0x100df3a4`, read only by `WeaponInfo`).
    pub thirdperson_switch: bool,
    /// `cl_thirdperson` (`0x100df544`, read only by `Spectator`).
    pub cl_thirdperson: f32,
    pub saytext_time: f32,
    /// `hud_deathnotice_time`. No handler here reads it any more (`DeathMsg` is handled by
    /// `hud::notice::on_death_msg_display`, which reads the cvar itself); kept so callers building a
    /// `Ctx` elsewhere (the menus) stay unchanged.
    pub deathnotice_time: f32,
    /// `Key_LookupBinding` (`gEngfuncs` slot `0x100d5c4c`), for `ShowMenu`'s `KB_ConvertString`.
    pub key_binding: &'a dyn Fn(&[u8]) -> Option<Vec<u8>>,
}

/// A [`Ctx::key_binding`] with no key bound to anything.
pub fn no_key_binding(_: &[u8]) -> Option<Vec<u8>> {
    None
}

#[derive(Debug, Clone)]
pub struct Hud {
    // ---- CHudTSStatus
    /// `CHudTSStatus::m_iHealth` (`+0x20`), drawn by the status panel; its `Reset` clears it.
    pub health: u8,
    /// `0x100cf9d8`: the last `TSHealth` value as an int. Only the `TSHealth` handler writes it
    /// (`0x1004b747`; `ResetHUD` leaves it), and `0x10026860` (`<= 0`) reads it for
    /// `CL_ButtonBits`' forced +score (`0x10026936`) and the prediction's `ItemPostFrame` gate
    /// (`0x10003d54`).
    pub last_hp: i32,
    pub armor: u8,
    pub stance: u8,
    pub cash: i32,
    pub slots: u8,
    pub pw_flags: u32,
    pub pw_value: i32,
    /// `DrawPowerup`'s name buffer (`0x1018a5e0`), never cleared.
    pub pw_label: String,
    pub pw_expire: f32,
    pub kungfu_icon: bool,
    pub move_icon: bool,
    pub round_time: f32,
    pub round_next: f32,
    pub round_mode: u8,
    pub observer: bool,
    pub messages: Vec<TsLine>,
    pub objectives: Vec<Objective>,
    pub obj_max: i32,
    pub flags: [FlagState; 16],
    pub flag_count: i32,
    /// The flag panel's blink timers and phases (`+0x1530`/`+0x152c` every 0.1 s,
    /// `+0x1534`/`+0x152d` every 0.5 s).
    pub flag_blink: [(f32, bool); 2],
    // ---- CHudTSWeapons
    pub grid: [[Slot; GRID_BUCKETS]; GRID_ROWS],
    pub menu_open: bool,
    pub menu_row: i32,
    pub menu_bucket: i32,
    pub reserve: [i32; 16],
    pub firemode: u8,
    pub last_firemode: u8,
    pub clip: i32,
    pub weapon: i32,
    pub attachments: u32,
    pub active_items: u32,
    pub next_stunt: f32,
    /// `m_flTime` when the last `NextStunt` arrived, and its tenths (0, 0 after a reset): for a
    /// ruleset that times the cooldown from the message instead of running `next_stunt` down
    /// ([`crate::gameplay::ClientRules::stunt_cooldown`]).
    pub next_stunt_msg: (f32, u8),
    /// `m_flTime` at the weapon element's last `Think` (0 after a reset), for
    /// [`crate::gameplay::ClientRules::kungfu_regen`].
    pub think_time: f32,
    pub wires: [u8; 3],
    pub kfu_power: f32,
    pub kfu_prev: f32,
    pub invnext_time: f32,
    pub last_laser_color: i32,
    pub usepowerup_time: f32,
    pub custom_weapons: Vec<p::CustomWp>,
    // ---- game / viewport
    pub gamemode: i32,
    pub realbullet: i32,
    pub cashbased: i32,
    pub ammocount: i32,
    pub restriction: i32,
    /// `SrvSett` key 4 (port addition): the server's gameplay ruleset id, `None` from each
    /// `InitHUD` until the server sends one ([`crate::gameplay::hud_rules`]). The server chooses
    /// it at a map start, so it only changes with a map (or a server).
    pub gameplay: Option<i16>,
    /// The last ruleset reported on the console (`Effect::GameplayRuleset`); kept across maps.
    pub gameplay_seen: Option<i16>,
    /// `SrvSett` keys 5..7 (port addition, `shared::protocol::port`): the server library's
    /// version, `None` from each `InitHUD` until the server sends key 5 (TS 3.0's server never
    /// does). Shown in the buy menu.
    pub server_version: Option<[i16; 3]>,
    pub bullettime: bool,
    pub players: Vec<PlayerExtra>,
    pub team_scores: Vec<TeamScore>,
    pub team_names: Vec<String>,
    pub server_name: String,
    pub motd: String,
    pub motd_final: bool,
    /// `CHudGeiger` (`+0x1a4`): `m_iGeigerRange` and `HUD_ACTIVE`, set by the first `Geiger`.
    pub geiger_range: i32,
    pub geiger_active: bool,
    /// The CRT's `rand` state, shared by the Geiger clicks and the HudText flicker.
    pub crt: super::message::CrtRand,
    // ---- SDK elements kept by TS
    /// `CHudDeathNotice`.
    pub deaths: super::notice::DeathNotices,
    /// `CHudMessage` (`HudText`, `GameTitle`).
    pub message: super::message::HudMessage,
    /// `CHudStatusBar` (`StatusText`, `StatusValue`).
    pub status_bar: super::statusbar::StatusBar,
    /// `CHudAmmoSecondary` (`SecAmmoVal`, `SecAmmoIcon`).
    pub sec_ammo: super::tfc::AmmoSecondary,
    /// The viewport's TFC message values (`ValClass` … `AllowSpec`).
    pub tfc: super::tfc::ViewportTfc,
    /// `CHudSayText`.
    pub say_text: super::saytext::SayText,
    /// `CHudStatusIcons`.
    pub icons: super::icons::StatusIcons,
    /// `CHud::m_iConcuss` (`+0x1c`, the `Concuss` message), `v_idlescale`'s source.
    pub concuss: i32,
    /// `CHud::m_iLogo` (`+0x8`, the `Logo` message).
    pub logo: i32,
    pub center_text: String,
    pub center_expire: f32,
    pub menu: TextMenu,
    pub crosshair_id: Option<(i32, u8)>,
    pub intermission: bool,
    /// `CHudTrain::m_iPos` (`Train`): speed notch + 1 of the train being driven, 0 = hidden.
    pub train_pos: u8,
    /// `gHUD.m_iHideHUDDisplay` (`HideWeapon`): HIDEHUD_WEAPONS 1 (tank in use), HIDEHUD_ALL 4.
    pub hide_hud: u8,
    /// `gHUD.m_vecOrigin` (3.0 `0x100cf958`, 2.1 `0x102da400`): `cdata->origin` as
    /// `CHud::UpdateClientData` (3.0 `0x10024b00`, 2.1 `0x1004e060`) last copied it, the local
    /// player's predicted origin.
    pub vec_origin: [f32; 3],
}

impl Default for Hud {
    fn default() -> Self {
        let mut h = Hud {
            health: 0,
            last_hp: 0,
            armor: 0,
            stance: 0,
            cash: 0,
            slots: 0,
            pw_flags: 0,
            pw_value: 0,
            pw_label: String::new(),
            pw_expire: 0.0,
            kungfu_icon: false,
            move_icon: false,
            round_time: 0.0,
            round_next: 0.0,
            round_mode: 0,
            observer: false,
            messages: vec![TsLine::default(); MAX_TS_MESSAGES],
            objectives: vec![Objective::default(); MAX_OBJECTIVES],
            obj_max: 0,
            flags: [FlagState::default(); 16],
            flag_count: 0,
            flag_blink: [(0.0, false); 2],
            grid: [[EMPTY; GRID_BUCKETS]; GRID_ROWS],
            menu_open: false,
            menu_row: 0,
            menu_bucket: 0,
            reserve: [0; 16],
            firemode: 0,
            last_firemode: 0,
            clip: 0,
            weapon: 0,
            attachments: 0,
            active_items: 0,
            next_stunt: 0.0,
            next_stunt_msg: (0.0, 0),
            think_time: 0.0,
            wires: [0; 3],
            kfu_power: 100.0,
            kfu_prev: 100.0,
            invnext_time: 0.0,
            last_laser_color: -1,
            usepowerup_time: 0.0,
            custom_weapons: Vec::new(),
            gamemode: 0,
            realbullet: 0,
            cashbased: 0,
            ammocount: 0,
            // `0x1018d174` is zero (`.bss`) until the server's `SrvSett 3` (sent in `InitHUD`),
            // which blocks every weapon in the buy menu until then, as in TS.
            restriction: 0,
            gameplay: None,
            gameplay_seen: None,
            server_version: None,
            bullettime: false,
            players: vec![PlayerExtra::default(); MAX_CLIENTS + 1],
            team_scores: Vec::new(),
            team_names: Vec::new(),
            server_name: String::new(),
            motd: String::new(),
            motd_final: false,
            geiger_range: 0,
            geiger_active: false,
            crt: super::message::CrtRand(1),
            deaths: Default::default(),
            message: Default::default(),
            status_bar: Default::default(),
            sec_ammo: Default::default(),
            tfc: Default::default(),
            say_text: Default::default(),
            icons: Default::default(),
            concuss: 0,
            logo: 0,
            center_text: String::new(),
            center_expire: 0.0,
            menu: TextMenu::default(),
            crosshair_id: None,
            intermission: false,
            train_pos: 0,
            hide_hud: 0,
            vec_origin: [0.0; 3],
        };
        h.reset_weapons();
        h
    }
}

/// Grid position (row, bucket) of a weapon: `row = info.pos (+0x85)`, `bucket = info.slot
/// (+0x84)`. TS skips ids whose position is outside the grid.
pub fn grid_pos(id: i32) -> Option<(usize, usize)> {
    let w = shared::weapons::tables().weapon(u8::try_from(id).ok()?)?;
    let (bucket, row) = (w.hud[0] as usize, w.hud[1] as usize);
    (row < GRID_ROWS && bucket < GRID_BUCKETS).then_some((row, bucket))
}

impl Hud {
    /// The weapon id in the slot grid at (`row`, `bucket`), -1 for an empty or out-of-grid cell.
    pub fn slot_id(&self, row: i32, bucket: i32) -> i32 {
        let (Ok(r), Ok(b)) = (usize::try_from(row), usize::try_from(bucket)) else { return -1 };
        self.grid.get(r).and_then(|r| r.get(b)).map_or(-1, |s| s.id)
    }

    /// `0x100581c0`, which the prediction runs through `0x1000acd0` at the end of a first
    /// prediction with a weapon record (`0x100040dc..0x100040ef`): the record's clip (`+0x78c`),
    /// fire mode (`+0x78a`) and attachments (`+0x798`) go straight into the element, the weapon's
    /// menu cell becomes `{id, clip}` and its akimbo and single twins' cells take their records'
    /// clips. The reserve (`+0x748`) is left to `WeaponInfo`. Kung fu (no record) writes nothing.
    pub fn predicted_weapon(&mut self, gun: &shared::weapons::Gun) {
        let Some(s) = gun.slot() else { return };
        let id = gun.current;
        self.clip = s.clip;
        self.firemode = s.mode;
        self.attachments = s.items;
        if let Some((r, b)) = grid_pos(id as i32) {
            self.grid[r][b] = Slot { id: id as i32, clip: s.clip };
        }
        let Some(w) = shared::weapons::info(id) else { return };
        // `0x1005824d`: the akimbo twin as it is; `0x1005829a`: the single one unless it's `id`.
        for twin in [w.akimbo, if w.single == id as i32 { -1 } else { w.single }] {
            if let Some((r, b)) = (0..MAX_WEAPONS as i32).contains(&twin).then(|| grid_pos(twin)).flatten() {
                self.grid[r][b].clip = gun.status[twin as usize].clip;
            }
        }
    }

    /// `CHudTSWeapons::Reset` (1004cd60): empty grid, kung fu in its slot.
    pub fn reset_weapons(&mut self) {
        self.grid = [[EMPTY; GRID_BUCKETS]; GRID_ROWS];
        self.menu_open = false;
        self.weapon = 0;
        self.menu_row = 0;
        self.menu_bucket = 0;
        if let Some((r, b)) = grid_pos(0) {
            self.grid[r][b] = Slot { id: 0, clip: 1 };
        }
        self.active_items = 0;
        if let Some(power) = crate::gameplay::hud_rules(self.gameplay).kungfu_power_on_reset() {
            self.kfu_power = power;
        }
        self.next_stunt = 0.0;
        self.next_stunt_msg = (0.0, 0);
        self.think_time = 0.0;
        self.invnext_time = 0.0;
        self.wires = [0; 3];
        self.usepowerup_time = 0.0;
        self.last_laser_color = -1;
    }

    /// The fields `CHudTSWeapons::VidInit` (`0x1004cb50`; 2.1 `0x100894c0` does the same) sets,
    /// after [`Hud::reset`] in `HUD_VidInit`. Unlike `Reset` it leaves the grid empty, kung fu
    /// included (`+0x164`, all −1), and also clears the weapon panel's state: `+0x788..+0x7b8`
    /// (fire mode `+0x78a`, clip `+0x78c`, attachments `+0x798`), `+0x7b8` the weapon id and
    /// `+0x748` the reserve ammo per type. Kung-fu power and its copy (`+0x3700/+0x3704`) are
    /// 100. The buy menu (`+0x7c0`) is re-initialised by `menus::vid_init`, the think sounds'
    /// state (`+0x3708..+0x3714`) by `weapons::reset_think_sounds`, `+0x3724` by the caller.
    ///
    /// It starts by rebuilding the client's weapon table (3.0 `FUN_10007de0`, 2.1 `FUN_1001e270`:
    /// zeroed, then every weapon's info builder again), which drops every `CustomWP` value
    /// (issue #248): the custom tables go, and with them the list of received values.
    pub fn vid_init_weapons(&mut self) {
        shared::weapons::table::reset_custom_tables();
        self.custom_weapons.clear();
        self.reset_weapons();
        self.grid = [[EMPTY; GRID_BUCKETS]; GRID_ROWS];
        self.reserve = [0; 16];
        self.firemode = 0;
        self.clip = 0;
        self.attachments = 0;
        self.kfu_power = 100.0;
        self.kfu_prev = 100.0;
    }

    /// `CHudTSStatus::Reset` (10049e70).
    /// `CHudTSStatus::Reset`.
    pub fn reset_status(&mut self) {
        self.stance = 0;
        self.health = 0;
        self.armor = 0;
        self.kungfu_icon = false;
        self.move_icon = false;
        self.pw_flags = 0;
        self.pw_expire = 0.0;
        self.round_time = 0.0;
        self.round_next = 0.0;
        self.round_mode = 0;
    }

    /// `ResetHUD`: every element's `Reset`.
    pub fn reset(&mut self) {
        self.reset_status();
        self.reset_weapons();
        // CHudAmmo::Reset clears m_iHideHUDDisplay; CHudTrain has no Reset.
        self.hide_hud = 0;
        self.menu = TextMenu::default();
    }

    /// `InitHUD`: `InitHUDData` (death notices and say text cleared).
    pub fn init_hud_data(&mut self) {
        self.deaths.clear();
        self.say_text.clear();
        self.menu = TextMenu::default();
    }

    pub fn ammo_type(&self, id: i32) -> usize {
        shared::weapons::tables().weapon(id.max(0) as u8).map_or(0, |w| w.ammo as usize).min(15)
    }

    /// `AddTSMessage` (1004b520), with the ruleset's text and placement
    /// ([`ClientRules::ts_message_layout`]).
    pub fn add_ts_message(&mut self, ctx: &Ctx, text: &str, rgb: [i32; 3], tenths: i32, flags: u8) {
        let layout = crate::gameplay::hud_rules(self.gameplay).ts_message_layout();
        let now = ctx.now;
        // `0x1004b52d..0x1004b559`: the first of slots 0..14 that has expired, else slot 15 if it
        // isn't still showing.
        let i = self.messages.iter().take(15).position(|m| m.expire < now).unwrap_or(15);
        if self.messages.get(i).is_none_or(|m| m.expire > now) {
            return;
        }
        let text = if layout.lookup_and_highlight { localise_with(ctx.titles, text) } else { text.to_owned() };
        let mut line = TsLine { text: text.chars().take(layout.max_chars).collect(), flags, ..Default::default() };
        let highlight = layout.lookup_and_highlight && !ctx.local_name.is_empty() && line.text.contains(&ctx.local_name);
        line.rgb = if highlight { [155; 3] } else { rgb };
        let (w, h) = (ctx.screen.width, ctx.screen.height);
        line.y = if flags & 1 != 0 {
            32
        } else if flags & 8 != 0 {
            h - layout.bottom_margin
        } else {
            h / 2
        };
        line.x = if flags & 2 != 0 {
            32
        } else if let Some(cw) = layout.char_width {
            // `strlen` is a `size_t`: the products wrap as C's do.
            let n = line.text.chars().count() as i32;
            if flags & 4 != 0 { w.wrapping_sub(n.wrapping_mul(cw)).wrapping_sub(32) } else { w / 2 + (n >> 1).wrapping_mul(-cw) }
        } else {
            let len = super::text::string_len(ctx.screen, &line.text);
            if flags & 4 != 0 { w - len - 32 } else { w / 2 - len / 2 }
        };
        // Stack below/above messages already at the same spot, or replace one (flag 0x10).
        let mut moved = 0;
        let mut j = 0;
        while j < MAX_TS_MESSAGES && moved <= 15 {
            let m = &self.messages[j];
            if j != i && m.expire > now && m.y == line.y && m.flags == line.flags {
                if flags & 0x10 != 0 {
                    self.messages[j].expire = 0.0;
                    break;
                }
                line.y += if line.flags & 1 != 0 { 16 } else { -16 };
                moved += 1;
                // `0x1004b6e6..0x1004b6ef`: TS resets the index to 0 and then increments it, so
                // the rescan starts at slot 1.
                j = 1;
                continue;
            }
            j += 1;
        }
        line.expire = if layout.expiry_double { (tenths as f32 as f64 / 10.0 + now as f64) as f32 } else { tenths as f32 * 0.1 + now };
        self.messages[i] = line;
    }

    /// Dispatch one message. Returns false if the payload was malformed.
    pub fn apply(&mut self, msg: Msg, buf: &[u8], ctx: &Ctx, fx: &mut Vec<Effect>) -> bool {
        macro_rules! dec {
            ($t:ty) => {
                match <$t>::decode(buf) {
                    Some(v) => v,
                    None => return false,
                }
            };
        }
        let now = ctx.now;
        match msg {
            // ---- CHudTSStatus
            Msg::TsHealth => {
                let v = dec!(p::TsHealth);
                self.health = v.hp;
                self.last_hp = v.hp as i32;
                if !self.observer && v.hp == 0 && !ctx.third_person {
                    fx.push(Effect::ToggleThirdPerson);
                }
            }
            Msg::TsArmor => self.armor = dec!(p::TsArmor).armor,
            Msg::TsState => self.stance = dec!(p::TsState).stance,
            Msg::TsCash => self.cash = dec!(p::TsCash).cash as i32,
            Msg::TsSpace => self.slots = dec!(p::TsSpace).slots,
            Msg::PwUp => {
                let v = dec!(p::PwUp);
                let flags = v.flags as u32;
                if flags & 0x104 == 0 {
                    self.pw_flags = flags;
                    self.pw_value = v.value as i32;
                } else if flags & 4 != 0 {
                    self.kungfu_icon = true;
                } else {
                    self.move_icon = true;
                }
            }
            Msg::PwUpTime => {
                let v = dec!(p::PwUpTime);
                self.pw_expire = now + v.secs as f32;
                self.pw_value = 0;
            }
            Msg::RoundTime => {
                let v = dec!(p::RoundTime);
                self.round_time = v.secs as f32;
                self.round_mode = v.mode;
                self.round_next = 0.0;
            }
            Msg::Objective => {
                let v = dec!(p::Objective);
                let idx = v.idx as usize;
                // TS's check is `(i >= 0) || (i < 32)`, always true; the port bounds it.
                if idx < MAX_OBJECTIVES {
                    if idx == 0 {
                        self.objectives = vec![Objective::default(); MAX_OBJECTIVES];
                        self.obj_max = 0;
                    }
                    self.objectives[idx] = Objective { visible: true, pos: [v.x, v.y, v.z], label: v.label.chars().take(63).collect() };
                    self.obj_max = self.obj_max.max(idx as i32);
                }
            }
            Msg::ShowObj => {
                let v = dec!(p::ShowObj);
                if let Some(o) = self.objectives.get_mut(v.idx as usize) {
                    o.visible = v.visible != 0;
                }
            }
            Msg::TsMessage => {
                let v = dec!(p::TsMessage);
                let tenths = (v.time as i32).min(100);
                self.add_ts_message(ctx, &v.text, [v.r as i32, v.g as i32, v.b as i32], tenths, v.flags);
            }
            Msg::PTakeDam => {
                let v = dec!(p::PTakeDam);
                fx.push(Effect::Fx(crate::state::TsEffect::PlayerHit {
                    pos: [v.px, v.py, v.pz],
                    dir: [v.dx, v.dy, v.dz],
                    amount: v.amount,
                }));
            }
            Msg::TsSlowMo => {
                let v = dec!(p::TsSlowMo);
                let mut target = v.factor100 * 0.01;
                if target == 0.0 {
                    target = 1.0;
                }
                let (current, prev) = crate::state::with(|s| {
                    let c = (s.time_scale, s.slowmo_factor100);
                    s.time_scale_target = target;
                    s.slowmo_factor100 = v.factor100;
                    c
                });
                if crate::gameplay::hud_rules(self.gameplay).go_slow_sound(current, target, prev, v.factor100) {
                    fx.push(Effect::GoSlow);
                }
            }
            Msg::TsFrags => fx.push(Effect::Fx(crate::state::TsEffect::Frags(dec!(p::TsFrags)))),
            Msg::TsPAward => {
                let v = dec!(p::TsPAward);
                if (1..=MAX_CLIENTS as u8).contains(&v.client) {
                    self.players[v.client as usize].award = v.award;
                }
            }
            Msg::SrvSett => {
                let v = dec!(p::SrvSett);
                match v.key {
                    0 => self.realbullet = v.value as i32,
                    1 => self.cashbased = v.value as i32,
                    2 => self.ammocount = v.value as i32,
                    3 => self.restriction = v.value as i32,
                    ts_gameplay::SRVSETT_KEY => {
                        self.gameplay = Some(v.value);
                        if self.gameplay_seen.replace(v.value) != Some(v.value) {
                            fx.push(Effect::GameplayRuleset(v.value));
                        }
                    }
                    k if k == port::SRVSETT_VERSION_KEYS[0] => self.server_version = Some([v.value, 0, 0]),
                    k if k == port::SRVSETT_VERSION_KEYS[1] => {
                        if let Some(sv) = &mut self.server_version {
                            sv[1] = v.value;
                        }
                    }
                    k if k == port::SRVSETT_VERSION_KEYS[2] => {
                        if let Some(sv) = &mut self.server_version {
                            sv[2] = v.value;
                            fx.push(Effect::ServerVersion(*sv));
                        }
                    }
                    _ => {}
                }
            }
            Msg::TsBTime => self.bullettime = dec!(p::TsBTime).on != 0,
            Msg::TsHVoice => match dec!(p::TsHVoice).lost {
                0 => fx.push(Effect::Sound("misc/successful.wav")),
                1 => fx.push(Effect::Sound("misc/failed.wav")),
                _ => {}
            },
            Msg::TsFlag => {
                let v = dec!(p::TsFlag);
                if (1..16).contains(&v.idx) {
                    self.flags[v.idx as usize] =
                        FlagState { state: v.state, carrier: if v.state == 1 { v.carrier } else { 0 }, team: v.team };
                }
                self.flag_count = 16;
            }
            Msg::TsMBlur => {
                let v = dec!(p::TsMBlur);
                fx.push(Effect::Fx(crate::state::TsEffect::MotionBlur { entity: v.entity as i32 }));
            }
            Msg::TsPSys => {
                let v = dec!(p::TsPSys);
                if v.kind == 3 {
                    fx.push(Effect::Fx(crate::state::TsEffect::Wind { seed: v.seed, yaw: v.yaw, strength: v.strength }));
                }
            }
            Msg::PlInfo => {
                let v = dec!(p::PlInfo);
                if (1..=MAX_CLIENTS as u8).contains(&v.client) && v.key == 0 {
                    self.players[v.client as usize].laser_color = v.value;
                }
            }
            Msg::TsFade => fx.push(Effect::Fade(dec!(p::TsFade))),
            // ---- CHudTSWeapons
            Msg::WeaponInfo => {
                // TS's `READ_BYTE` (`0x10029a10`) zero-extends, so every id byte, 0xff included, is
                // the new weapon; it keeps the old id (`0x1004f822`) only on -1, a read past the end,
                // and the port drops a short message instead.
                let v = dec!(p::WeaponInfo);
                let old = self.weapon;
                self.weapon = v.id as i32;
                self.clip = v.clip as i32;
                // `GetWeaponInfo` (`0x100071c0`) answers ids outside 0..38 with entry 0, so such an
                // id takes kung fu's ammo type and slot.
                let info_id = if (0..MAX_WEAPONS as i32).contains(&self.weapon) { self.weapon } else { 0 };
                let t = self.ammo_type(info_id);
                self.reserve[t] = v.reserve as i32;
                if v.id == 0 {
                    let rules = crate::gameplay::hud_rules(self.gameplay);
                    if rules.kungfu_weapon_info_resets_menu_selection() {
                        self.menu_row = 0;
                        self.menu_bucket = 0;
                    }
                    if let Some(power) = rules.kungfu_power_from_weapon_info(v.clip) {
                        self.kfu_power = power;
                    }
                }
                self.firemode = v.mode;
                self.attachments = v.attachments as u32;
                if self.weapon >= 1
                    && let Some((r, b)) = grid_pos(info_id)
                {
                    self.grid[r][b] = Slot { id: self.weapon, clip: self.clip };
                }
                // `cl_thirdpersonswitch` 1 (`0x1004f93b..0x1004f970`): kung fu goes to third person
                // whatever the old weapon was; a gun goes back to first person only when the
                // weapon before this message was kung fu (id 0), not on a gun-to-gun change.
                let toggle = if self.weapon < 1 { !ctx.third_person } else { ctx.third_person && old == 0 };
                if ctx.thirdperson_switch && toggle {
                    fx.push(Effect::ToggleThirdPerson);
                }
            }
            Msg::ClipInfo => {
                self.clip = dec!(p::ClipInfo).clip as i32;
                if self.weapon != 0
                    && let Some((r, b)) = grid_pos(self.weapon)
                {
                    self.grid[r][b].clip = self.clip;
                }
            }
            Msg::WStatus => {
                let v = dec!(p::WStatus);
                if let Some((r, b)) = grid_pos(v.id as i32) {
                    self.grid[r][b] = Slot { id: if v.owned == 0 { -1 } else { v.id as i32 }, clip: v.clip as i32 };
                }
            }
            Msg::ActItems => {
                let v = dec!(p::ActItems);
                if v.client <= 32 {
                    self.players[v.client as usize].active_items = v.bits;
                }
            }
            Msg::NextStunt => {
                let tenths = dec!(p::NextStunt).tenths;
                self.next_stunt = tenths as f32 * 0.1;
                self.next_stunt_msg = (now, tenths);
            }
            Msg::Wires => {
                let v = dec!(p::Wires);
                self.wires = [v.w0, v.w1, v.w2];
            }
            Msg::CustomWp => {
                // `0x1004fb50`: `sscanf("%i %f %f %f %f %i")` into the client's own weapon table
                // (`GetWeaponInfo(id)` +0x28 accuracy, +0x30 view kick, +0x34 accuracy kick,
                // +0x1c damage, +0xd8 cost), which prediction, the events and the buy menu read.
                // They last until TS rebuilds that table: at the next `CHudTSWeapons::VidInit`
                // ([`Hud::vid_init_weapons`]) and the gun spawn after it
                // (`prediction::gun::spawn_and_death`), issue #248. Guarded: an id outside the
                // table is ignored.
                let v = dec!(p::CustomWp);
                if let Ok(id) = u8::try_from(v.id) {
                    shared::weapons::table::set_weapon_params(id, v.accuracy, v.view_kick, v.acc_kick, v.damage, v.cost);
                }
                self.custom_weapons.retain(|c| c.id != v.id);
                self.custom_weapons.push(v);
            }
            Msg::KFuPower => {
                self.kfu_prev = self.kfu_power;
                self.kfu_power = (dec!(p::KFuPower).power as f32).clamp(-100.0, 100.0);
            }
            // ---- CHud / viewport
            // `0x100219a0`: every element's `Reset` (`CHudStatusIcons` too), then `m_iConcuss`
            // = 0; the caller zeroes the zoom sensitivity (`+0x18`).
            Msg::ResetHUD => {
                self.reset();
                self.icons.reset();
                self.message.reset();
                self.status_bar.reset();
                self.sec_ammo.reset();
                self.concuss = 0;
            }
            Msg::InitHUD => {
                self.init_hud_data();
                // Port addition: a new game on this server (a connect or a map), whose `SrvSett`
                // 4..7 follow. Not `HUD_VidInit`: the engine also calls that on a video mode
                // change.
                self.gameplay = None;
                self.server_version = None;
            }
            Msg::GameMode => {
                let mut r = p::Reader::new(buf);
                self.gamemode = r.byte() as i32;
            }
            // The caller applies it (`hud::set_fov`): the zoom lives in `crate::state`.
            Msg::SetFov => {}
            Msg::ScoreInfo => {
                let v = dec!(p::ScoreInfo);
                if (1..=MAX_CLIENTS as u8).contains(&v.client) {
                    let pl = &mut self.players[v.client as usize];
                    pl.frags = v.frags as i32;
                    pl.deaths = v.deaths as i32;
                    pl.class = v.class as i32;
                    // A negative team number is stored as 0 (`0x100650f6`).
                    pl.team = v.team.max(0) as i32;
                }
            }
            Msg::TeamInfo => {
                let v = dec!(p::TeamInfo);
                if (1..=MAX_CLIENTS as u8).contains(&v.client) {
                    // `strncpy(teamname, READ_STRING(), 16)` (viewport `TeamInfo` 0x100651c0, into
                    // the 16 bytes at `0x101901fe + i * 0x20`): a 16-byte name keeps all 16 (TS
                    // leaves it unterminated and reads on into the next entry; the port stops).
                    // One char per byte, so a 0xE9 stays 0xE9 (#188).
                    self.players[v.client as usize].teamname = v.team.chars().take(16).collect();
                }
            }
            Msg::TeamScore => {
                let v = dec!(p::TeamScore);
                match self.team_scores.iter_mut().find(|t| t.name.eq_ignore_ascii_case(&v.team)) {
                    Some(t) => {
                        t.frags = v.frags as i32;
                        t.deaths = v.deaths as i32;
                    }
                    None => self.team_scores.push(TeamScore { name: v.team, frags: v.frags as i32, deaths: v.deaths as i32 }),
                }
            }
            Msg::TeamNames => {
                let Some(mut names) = p::decode_team_names(buf) else { return false };
                // `0x10064df0` stores only teams 1..4 (64-byte names at `+0x8fc`); the rest are
                // skipped, so the team menu never sees more than four.
                names.truncate(MAX_TEAM_NAMES);
                self.team_names = names;
                for n in &self.team_names {
                    if !self.team_scores.iter().any(|t| t.name.eq_ignore_ascii_case(n)) {
                        self.team_scores.push(TeamScore { name: n.clone(), ..Default::default() });
                    }
                }
            }
            Msg::ServerName => self.server_name = p::Reader::new(buf).string(),
            // `CHudGeiger::MsgFunc_Geiger` (`0x1001f4a0`): the range is the byte times 4.
            Msg::Geiger => {
                let mut r = p::Reader::new(buf);
                self.geiger_range = (r.byte() as i32) << 2;
                self.geiger_active = true;
            }
            Msg::Motd => {
                let v = dec!(p::Motd);
                if self.motd_final {
                    self.motd.clear();
                    self.motd_final = false;
                }
                // Viewport buffer at +0x240 holds 0x600 bytes.
                let room = 0x5ffusize.saturating_sub(self.motd.len());
                self.motd.extend(v.text.chars().take(room));
                if v.last != 0 {
                    self.motd_final = true;
                    fx.push(Effect::OpenMotd);
                }
            }
            Msg::Spectator => {
                let v = dec!(p::Spectator);
                let cl = v.client as usize;
                if (1..=MAX_CLIENTS).contains(&cl) {
                    // TS keeps the raw byte (`0x101900ec[i]`, copied to `0x101901f5`), but its
                    // readers, `HUD_AddEntity` (`0x10044846`) and the scoreboard (`0x1005e3e8`),
                    // only test it for zero; the local `== 1` test below is the one that differs.
                    self.players[cl].observer = v.observer != 0;
                    self.players[cl].active_items = 0;
                }
                // The local player (`10065294..100652f8`), on every message, not only on a change:
                // observer (`0x100d4401`) is `observer == 1`. Whether the camera toggles is the
                // ruleset's (`ClientRules::spectator_toggles_third_person`); entering opens the
                // buy menu.
                if cl as i32 == ctx.local_index {
                    self.observer = v.observer == 1;
                    let rules = crate::gameplay::hud_rules(self.gameplay);
                    if rules.spectator_toggles_third_person(self.observer, ctx.third_person, ctx.cl_thirdperson) {
                        fx.push(Effect::ToggleThirdPerson);
                    }
                    if self.observer {
                        fx.push(Effect::OpenBuyMenu);
                    }
                    fx.push(Effect::ResetSpread);
                }
            }
            Msg::VguiMenu => {
                let mut r = p::Reader::new(buf);
                let id = r.byte();
                if r.bad {
                    return false;
                }
                // `0x10064f00`: menu 4 carries the map's name (`strncpy` 64, `[63] = 0`); every id
                // goes to `ShowVGUIMenu` (`0x10064050`), which makes only 2, 4 and 5 here.
                let name = (id == 4).then(|| super::saytext::bytes(&r.string()).into_iter().take(63).collect());
                fx.push(Effect::VguiMenu(id as i32, name));
            }
            // `hud::notice::on_death_msg_display` handles it: it needs the engine's player names.
            Msg::DeathMsg => {
                dec!(p::DeathMsg);
            }
            // `MsgFunc_SayText` (`0x1002ecc0`): `SayTextPrint(text, size - 1, client)`.
            Msg::SayText => {
                let v = dec!(p::SayText);
                fx.push(Effect::SayText(v.text, v.client as i32));
            }
            // `MsgFunc_TextMsg` (`0x10034a60`): the destination, then five strings (missing ones
            // read as ""), each through `LookupString` (a whole-string `#key` only; the first
            // may change the destination), the four arguments losing one trailing newline
            // (`0x10034a10`). `sprintf(msg, arg1..arg4)`, then `\r` becomes `\n` (`0x10034a40`).
            // Guarded: TS copies the strings and the result into 128-byte static buffers with
            // `strcpy`/`sprintf` and overflows them on a long text; the port keeps them whole.
            Msg::TextMsg => {
                let v = dec!(p::TextMsg);
                let read = |i: usize| super::saytext::bytes(v.strings.get(i).map_or("", String::as_str));
                let mut dest = v.dest as i32;
                let msg = super::saytext::string(&lookup_string(ctx.titles, &read(0), Some(&mut dest)));
                let args: Vec<String> = (1..5)
                    .map(|i| {
                        let mut a = lookup_string(ctx.titles, &read(i), None);
                        if matches!(a.last(), Some(b'\n' | b'\r')) {
                            a.pop();
                        }
                        super::saytext::string(&a)
                    })
                    .collect();
                let a: Vec<super::text::Arg> = args.iter().map(|s| super::text::Arg::S(s)).collect();
                let text = super::text::format(&msg, &a).replace('\r', "\n");
                match dest {
                    // HUD_PRINTNOTIFY: a leading 1 sends it to the notify area.
                    1 => fx.push(Effect::ConsolePrint(format!("\u{1}{text}"))),
                    2 => fx.push(Effect::ConsolePrint(text)),
                    // HUD_PRINTTALK: `SayTextPrint(text, 128, -1)` (`0x10034c01`).
                    3 => fx.push(Effect::SayText(text, -1)),
                    4 => {
                        self.center_text = text.clone();
                        self.center_expire = now + 5.0;
                        fx.push(Effect::CenterPrint(text));
                    }
                    _ => {}
                }
            }
            // `CHudMessage::MsgFunc_HudText` (`0x100295c0`). A short message is TS's empty
            // `READ_STRING`, a custom message with no text.
            Msg::HudText => {
                let text = super::saytext::bytes(&p::Reader::new(buf).string());
                self.message.msg_hud_text(&text, now, ctx.titles);
            }
            // `CHudMessage::MsgFunc_GameTitle` (`0x10029610`, hooked at `0x100288e0`).
            Msg::GameTitle => self.message.msg_game_title(now, ctx.titles),
            // CHudTrain::MsgFunc_Train (10034df0).
            Msg::Train => self.train_pos = dec!(p::Train).pos,
            // CHudAmmo::MsgFunc_HideWeapon (1001a110). TS only resets the engine crosshair,
            // which it doesn't use; the port keeps the bits for HIDEHUD_ALL.
            Msg::HideWeapon => self.hide_hud = dec!(p::HideWeapon).flags,
            // `CHudMenu::MsgFunc_ShowMenu` (`0x10028760`): the slots and the shutoff time take
            // effect at once. No slots hides the menu. Otherwise the text starts or (after a chunk
            // that said more follows) extends the prelocalised string, kept to 511 bytes; the
            // last chunk localises it (`BufferedLocaliseTextString`). The menu shows after every
            // chunk, with the last localised text until the new one is complete.
            Msg::ShowMenu => {
                let v = dec!(p::ShowMenu);
                self.menu.slots = v.slots;
                self.menu.shutoff = if v.time > 0 { now + v.time as f32 } else { 0.0 };
                if v.slots == 0 {
                    self.menu.displayed = false;
                    self.menu.waiting = v.more != 0;
                    return true;
                }
                if !self.menu.waiting {
                    self.menu.pending.clear();
                }
                self.menu.pending.push_str(&v.text);
                self.menu.pending = self.menu.pending.chars().take(0x1ff).collect();
                if v.more == 0 {
                    // 0x10028843: `KB_ConvertString` swaps each `+command` for its key, and TS
                    // always takes the result.
                    let text = super::saytext::bytes(&localise_with(ctx.titles, &self.menu.pending));
                    self.menu.text = super::saytext::string(&crate::input::kb_convert_string(&text, ctx.key_binding));
                }
                self.menu.displayed = true;
                self.menu.waiting = v.more != 0;
            }
            // Stock/TFC bookkeeping messages TS keeps hooked; their elements draw nothing.
            Msg::CurWeapon
            | Msg::WeaponList
            | Msg::AmmoPickup
            | Msg::WeapPickup
            | Msg::ItemPickup
            | Msg::AmmoX
            | Msg::Health
            | Msg::Battery
            | Msg::Damage
            | Msg::SelAmmo => {}
            // Engine-handled; never hooked.
            Msg::ScreenShake | Msg::ScreenFade => {}
            // Port additions: the client extensions' channels (`crate::ext::on_tsext`) and the
            // plugin offers (`super::tspkg`), never the HUD's.
            Msg::TsExt | Msg::TsPkg => {}
        }
        true
    }

    /// `CHudMenu::SelectMenuItem`: returns the `menuselect` command if the slot is valid.
    pub fn menu_select(&mut self, slot: i32) -> Option<String> {
        if !self.menu.displayed || !(1..=10).contains(&slot) {
            return None;
        }
        if self.menu.slots & (1 << (slot - 1)) == 0 {
            return None;
        }
        self.menu.displayed = false;
        Some(format!("menuselect {slot}\n"))
    }

    /// The stunt cooldown state the divesteps colour by.
    pub fn stunt_cooldown(&self) -> StuntCooldown {
        StuntCooldown { remaining: self.next_stunt, received_at: self.next_stunt_msg.0, tenths: self.next_stunt_msg.1 }
    }

    /// Kung-fu power regenerates client-side between `KFuPower` updates (`Think` 1004ce10), by
    /// the ruleset's rule ([`ClientRules::kungfu_regen`]): in TS 3.0 15/s when non-negative, 1/s
    /// while negative, over frame time × time scale (`step`). `now` is `m_flTime`.
    pub fn regen_kungfu(&mut self, rules: &dyn ClientRules, step: f32, now: f32, slowmo_factor100: f32) {
        self.kfu_power = rules.kungfu_regen(KungfuRegen { power: self.kfu_power, step, now, last: self.think_time, slowmo_factor100 });
        self.think_time = now;
    }

    /// `DrawRoundTimer` tick (1004a130): one second (or 0.1 s in mode 1) per tick; frozen in
    /// mode 2. Returns the seconds to show.
    pub fn tick_round_timer(&mut self, now: f32) -> i32 {
        if self.round_next < now {
            if self.round_mode != 2 {
                self.round_time -= 1.0;
            }
            if self.round_time <= 0.0 {
                self.round_time = 0.0;
            }
            self.round_next = now + if self.round_mode == 1 { 0.1 } else { 1.0 };
        }
        self.round_time as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::hud::{
        KFuPower, Motd, PwUp, PwUpTime, ScoreInfo, ShowMenu, ShowObj, Spectator, TeamInfo, TextMsg, TsCash, TsHealth, TsMessage, TsPAward,
        TsSpace, TsState, WStatus, WeaponInfo,
    };

    struct NoTitles;
    impl Titles for NoTitles {
        fn get(&self, key: &str) -> Option<String> {
            match key {
                "HUD_Credits" => Some("Credits".to_owned()),
                "Notify" => Some("Note %s".to_owned()),
                _ => None,
            }
        }
        fn effect(&self, key: &str) -> i32 {
            if key == "Notify" { -1 } else { 0 }
        }
    }

    /// `CHudTSWeapons::VidInit` (`0x1004cb50`): unlike `Reset`, no kung-fu slot, and the weapon
    /// panel's fire mode, clip, attachments and reserve are cleared; power back to 100.
    #[test]
    fn weapons_vid_init_clears_the_panel() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud::default();
        h.reset_weapons();
        assert!(h.grid.iter().flatten().any(|s| s.id == 0), "Reset puts kung fu in its slot");
        h.reserve = [5; 16];
        (h.firemode, h.clip, h.attachments, h.weapon) = (2, 30, 3, 13);
        (h.kfu_power, h.kfu_prev, h.menu_open, h.last_laser_color) = (20.0, 30.0, true, 4);
        h.hide_hud = 1;
        // A `CustomWP` value (issue #248): the rebuilt weapon table drops it.
        assert!(shared::weapons::table::set_weapon_params(13, 0.5, 1.0, 1.0, 99.0, 1));
        h.custom_weapons.push(p::CustomWp { id: 13, accuracy: 0.5, view_kick: 1.0, acc_kick: 1.0, damage: 99.0, cost: 1 });
        h.vid_init_weapons();
        assert!(!shared::weapons::table::custom_tables_active(), "VidInit rebuilds the weapon table");
        assert!(h.custom_weapons.is_empty());
        assert!(h.grid.iter().flatten().all(|s| *s == EMPTY), "VidInit leaves the grid all -1");
        assert_eq!(h.reserve, [0; 16]);
        assert_eq!((h.firemode, h.clip, h.attachments, h.weapon), (0, 0, 0, 0));
        assert_eq!((h.kfu_power, h.kfu_prev, h.menu_open, h.last_laser_color), (100.0, 100.0, false, -1));
        assert_eq!(h.hide_hud, 1, "only CHudAmmo::Reset clears HideWeapon");
    }

    fn ctx(screen: &Screen) -> Ctx<'_> {
        Ctx {
            now: 10.0,
            screen,
            titles: &NoTitles,
            local_index: 1,
            local_name: "me".into(),
            third_person: false,
            thirdperson_switch: false,
            cl_thirdperson: 0.0,
            saytext_time: 5.0,
            deathnotice_time: 6.0,
            key_binding: &no_key_binding,
        }
    }

    #[test]
    fn status_messages() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        assert!(h.apply(Msg::TsHealth, &TsHealth { hp: 73 }.encode(), &c, &mut fx));
        assert!(h.apply(Msg::TsCash, &TsCash { cash: 1200 }.encode(), &c, &mut fx));
        assert!(h.apply(Msg::TsSpace, &TsSpace { slots: 61 }.encode(), &c, &mut fx));
        assert!(h.apply(Msg::TsState, &TsState { stance: 2 }.encode(), &c, &mut fx));
        assert_eq!((h.health, h.cash, h.slots, h.stance), (73, 1200, 61, 2));
        assert!(fx.is_empty());
        h.reset();
        assert_eq!((h.health, h.last_hp), (0, 73), "ResetHUD leaves 0x100cf9d8");
        assert!(h.apply(Msg::TsHealth, &TsHealth { hp: 0 }.encode(), &c, &mut fx));
        assert_eq!(fx, [Effect::ToggleThirdPerson], "death cam");
        assert_eq!(h.last_hp, 0);
        assert!(!h.apply(Msg::TsCash, &[1], &c, &mut fx), "short payload rejected");
    }

    /// `SrvSett` 4 (port addition): each `InitHUD` clears the server's ruleset and the `SrvSett` 4
    /// after it records it; a ruleset other than the last one seen is reported once. A video mode
    /// change (`ResetHUD`/`HUD_VidInit`) doesn't forget it.
    #[test]
    fn gameplay_ruleset_per_map() {
        ts30_gameplay::tables::make_default();
        use shared::protocol::hud::SrvSett;
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        let ruleset = |value: i16| SrvSett { key: ts_gameplay::SRVSETT_KEY, value }.encode();
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        h.apply(Msg::SrvSett, &ruleset(30), &c, &mut fx);
        assert_eq!((h.gameplay, &fx[..]), (Some(30), &[Effect::GameplayRuleset(30)][..]), "connect: recorded, reported");
        fx.clear();
        h.reset();
        assert_eq!(h.gameplay, Some(30));
        // The next map runs 2.1, the one after it 2.1 again, then 3.0.
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        assert_eq!(h.gameplay, None, "InitHUD clears it");
        h.apply(Msg::SrvSett, &ruleset(21), &c, &mut fx);
        assert_eq!((h.gameplay, &fx[..]), (Some(21), &[Effect::GameplayRuleset(21)][..]));
        fx.clear();
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        h.apply(Msg::SrvSett, &ruleset(21), &c, &mut fx);
        assert!(fx.is_empty(), "the same ruleset again isn't reported");
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        h.apply(Msg::SrvSett, &ruleset(30), &c, &mut fx);
        assert_eq!(fx, [Effect::GameplayRuleset(30)]);
        assert_eq!(h.gameplay, Some(30));
    }

    /// `SrvSett` 5..7 (port addition): the server's version, cleared by each `InitHUD`; keys 6
    /// and 7 without a key 5 first are ignored.
    #[test]
    fn server_version() {
        ts30_gameplay::tables::make_default();
        use shared::protocol::hud::SrvSett;
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        let set = |key: u8, value: i16| SrvSett { key, value }.encode();
        let [k5, k6, k7] = port::SRVSETT_VERSION_KEYS;
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        h.apply(Msg::SrvSett, &set(k6, 4), &c, &mut fx);
        h.apply(Msg::SrvSett, &set(k7, 4), &c, &mut fx);
        assert_eq!(h.server_version, None, "no key 5 yet");
        assert!(!fx.iter().any(|f| matches!(f, Effect::ServerVersion(_))), "nothing to compare");
        for (k, v) in [(k5, 3), (k6, 1), (k7, 12)] {
            h.apply(Msg::SrvSett, &set(k, v), &c, &mut fx);
        }
        assert_eq!(h.server_version, Some([3, 1, 12]));
        // Key 7 completes it: the client compares it with its own (`cl_version_strict`).
        assert_eq!(fx.iter().filter(|f| matches!(f, Effect::ServerVersion(_))).collect::<Vec<_>>(), [&Effect::ServerVersion([3, 1, 12])]);
        // TS 3.0's own keys are untouched.
        assert_eq!((h.realbullet, h.cashbased, h.ammocount), (0, 0, 0));
        h.apply(Msg::InitHUD, &[], &c, &mut fx);
        assert_eq!(h.server_version, None, "a map on TS 3.0's server sends none");
    }

    #[test]
    fn powerups() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::PwUp, &PwUp { flags: 1, value: 3 }.encode(), &c, &mut fx);
        assert_eq!((h.pw_flags, h.pw_value), (1, 3));
        h.apply(Msg::PwUp, &PwUp { flags: 4, value: 0 }.encode(), &c, &mut fx);
        assert!(h.kungfu_icon && !h.move_icon);
        assert_eq!(h.pw_flags, 1, "permanent flags don't replace the timed power-up");
        h.apply(Msg::PwUpTime, &PwUpTime { secs: 12 }.encode(), &c, &mut fx);
        assert_eq!((h.pw_expire, h.pw_value), (22.0, 0));
    }

    /// `MsgFunc_WeaponInfo` leaves the slot menu's selection on kung fu unless the ruleset resets
    /// it (`ClientRules::kungfu_weapon_info_resets_menu_selection`).
    #[test]
    fn kungfu_weapon_info_keeps_menu_selection() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud { menu_open: true, menu_row: 3, menu_bucket: 4, ..Hud::default() };
        let mut fx = Vec::new();
        h.apply(Msg::WeaponInfo, &WeaponInfo { id: 0, clip: 1, reserve: 0, mode: 0, attachments: 0 }.encode(), &c, &mut fx);
        assert_eq!((h.menu_open, h.menu_row, h.menu_bucket), (true, 3, 4));
    }

    #[test]
    fn weapon_grid() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        let (r, b) = grid_pos(0).unwrap();
        assert_eq!(h.grid[r][b], Slot { id: 0, clip: 1 });
        h.apply(Msg::WeaponInfo, &WeaponInfo { id: 13, clip: 30, reserve: 90, mode: 0, attachments: 8 }.encode(), &c, &mut fx);
        let (r, b) = grid_pos(13).unwrap();
        assert_eq!((r, b), (3, 4), "AK47 hud = [4, 3]");
        assert_eq!(h.grid[r][b], Slot { id: 13, clip: 30 });
        assert_eq!(h.reserve[6], 90, "7.62x39 is ammo type 6");
        h.apply(Msg::WeaponInfo, &WeaponInfo { id: 13, clip: 29, reserve: 90, mode: 1, attachments: 8 }.encode(), &c, &mut fx);
        assert_eq!((h.weapon, h.clip, h.firemode), (13, 29, 1));
        // 0xff is weapon 255, as in TS: entry 0's slot (kung fu) and ammo type take it.
        h.apply(Msg::WeaponInfo, &WeaponInfo { id: 0xff, clip: 7, reserve: 5, mode: 0, attachments: 0 }.encode(), &c, &mut fx);
        assert_eq!(h.weapon, 255);
        let (kr, kb) = grid_pos(0).unwrap();
        assert_eq!(h.grid[kr][kb], Slot { id: 255, clip: 7 });
        assert_eq!(h.grid[r][b], Slot { id: 13, clip: 29 });
        h.apply(Msg::WStatus, &WStatus { id: 13, owned: 0, clip: 0 }.encode(), &c, &mut fx);
        assert_eq!(h.grid[r][b].id, -1);
        h.apply(Msg::KFuPower, &KFuPower { power: -120 }.encode(), &c, &mut fx);
        assert_eq!(h.kfu_power, -100.0);
    }

    /// `0x100581c0`: the predicted record's clip, mode and attachments go into the element and the
    /// menu, the reserve and the weapon id don't; the twin's cell takes its clip; kung fu writes
    /// nothing. A mode change then plays the switch sound from `Think` (`h.firemode`).
    #[test]
    fn predicted_weapon_writes_the_element() {
        ts30_gameplay::tables::make_default();
        use shared::weapons::{Gun, Status, id, info};
        let mut h = Hud { weapon: id::BERETTA as i32, ..Default::default() };
        h.reserve[info(id::BERETTA).unwrap().ammo as usize] = 45;
        let mut g = Gun { current: id::BERETTA, ..Default::default() };
        g.status[id::BERETTA as usize] = Status { clip: 11, mode: 1, items: 3, ..Default::default() };
        g.status[id::BERETTA_AKIMBO as usize].clip = 24;
        h.predicted_weapon(&g);
        assert_eq!((h.weapon, h.clip, h.firemode, h.attachments), (id::BERETTA as i32, 11, 1, 3));
        assert_eq!(h.reserve[info(id::BERETTA).unwrap().ammo as usize], 45);
        let (r, b) = grid_pos(id::BERETTA as i32).unwrap();
        assert_eq!(h.grid[r][b], Slot { id: id::BERETTA as i32, clip: 11 });
        let (ar, ab) = grid_pos(id::BERETTA_AKIMBO as i32).unwrap();
        assert_eq!(h.grid[ar][ab], Slot { id: -1, clip: 24 }, "the twin's clip only");

        let before = (h.clip, h.firemode, h.attachments, h.grid);
        h.predicted_weapon(&Gun::default());
        assert_eq!((h.clip, h.firemode, h.attachments, h.grid), before, "kung fu: no record");
    }

    #[test]
    fn ts_messages_stack_and_replace() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        let m = TsMessage { r: 1, g: 2, b: 3, flags: 1, time: 30, text: "#HUD_Credits".into() };
        h.apply(Msg::TsMessage, &m.encode(), &c, &mut fx);
        h.apply(Msg::TsMessage, &m.encode(), &c, &mut fx);
        assert_eq!(h.messages[0].text, "Credits");
        assert_eq!(h.messages[0].y, 32);
        assert_eq!(h.messages[1].y, 48, "second top message goes below the first");
        assert!((h.messages[0].expire - 13.0).abs() < 1e-4, "time is in tenths");
        // 0x10 replaces an active message at the same spot with the same flags.
        let r = TsMessage { flags: 1 | 0x10, text: "x".into(), ..m.clone() };
        h.apply(Msg::TsMessage, &r.encode(), &c, &mut fx);
        h.apply(Msg::TsMessage, &r.encode(), &c, &mut fx);
        assert_eq!(h.messages[2].expire, 0.0, "first 0x11 message replaced");
        assert_eq!((h.messages[3].y, h.messages[3].text.as_str()), (32, "x"));
        let mine = TsMessage { flags: 0, text: "hello me".into(), ..m };
        h.apply(Msg::TsMessage, &mine.encode(), &c, &mut fx);
        assert!(h.messages.iter().any(|l| l.text == "hello me" && l.rgb == [155; 3]), "grey when it contains your name");
    }

    /// `AddTSMessage` (`0x1004b520`): slot 15 is the fallback, and after moving a message the
    /// rescan starts at slot 1.
    #[test]
    fn ts_message_slots_follow_ts() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut fx = Vec::new();
        let m = TsMessage { r: 1, g: 2, b: 3, flags: 1, time: 30, text: "x".into() };
        let mut h = Hud::default();
        for (k, l) in h.messages.iter_mut().enumerate().take(15) {
            *l = TsLine { expire: 100.0, y: 1000 + k as i32, flags: 2, ..Default::default() };
        }
        h.apply(Msg::TsMessage, &m.encode(), &c, &mut fx);
        assert_eq!((h.messages[15].text.as_str(), h.messages[15].y), ("x", 32), "slot 15 when 0..14 are busy");
        h.apply(Msg::TsMessage, &m.encode(), &c, &mut fx);
        assert_eq!(h.messages.iter().filter(|l| l.text == "x").count(), 1, "dropped with all sixteen busy");

        let mut h = Hud::default();
        h.messages[0] = TsLine { expire: 100.0, y: 48, flags: 1, ..Default::default() };
        h.messages[1] = TsLine { expire: 100.0, y: 32, flags: 1, ..Default::default() };
        h.apply(Msg::TsMessage, &m.encode(), &c, &mut fx);
        assert_eq!(h.messages[2].y, 48, "moved below slot 1, then slot 0 isn't checked again");
    }

    #[test]
    fn objectives_bounds_fixed() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::Objective, &p::Objective { x: 1.0, y: 2.0, z: 3.0, idx: 5, label: "Base".into() }.encode(), &c, &mut fx);
        assert!(h.objectives[5].visible);
        assert_eq!(h.obj_max, 5);
        h.apply(Msg::Objective, &p::Objective { idx: 200, label: "bad".into(), ..Default::default() }.encode(), &c, &mut fx);
        assert_eq!(h.obj_max, 5);
        h.apply(Msg::ShowObj, &ShowObj { idx: 5, visible: 0 }.encode(), &c, &mut fx);
        assert!(!h.objectives[5].visible);
        h.apply(Msg::Objective, &p::Objective { idx: 0, label: "a".into(), ..Default::default() }.encode(), &c, &mut fx);
        assert!(h.objectives[5].label.is_empty(), "idx 0 clears all");
    }

    #[test]
    fn scoreboard_messages() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::ScoreInfo, &ScoreInfo { client: 3, frags: 7, deaths: 2, class: 0, team: 1 }.encode(), &c, &mut fx);
        h.apply(Msg::TeamInfo, &TeamInfo { client: 3, team: "Specialists".into() }.encode(), &c, &mut fx);
        h.apply(Msg::TsPAward, &TsPAward { client: 3, award: 10 }.encode(), &c, &mut fx);
        h.apply(Msg::ScoreInfo, &ScoreInfo { client: 65, frags: 1, deaths: 1, class: 0, team: 0 }.encode(), &c, &mut fx);
        let p = &h.players[3];
        assert_eq!((p.frags, p.deaths, p.team, p.award), (7, 2, 1, 10));
        assert_eq!(p.teamname, "Specialists");
        h.apply(Msg::TeamScore, &p::TeamScore { team: "Specialists".into(), frags: 9, deaths: 3 }.encode(), &c, &mut fx);
        assert_eq!(h.team_scores[0].frags, 9);
    }

    /// #188: team names are the server's bytes: a CP1252 0xE9 stays one byte, and a 16-byte name
    /// (here cut by the server through a UTF-8 `é`, ending in C3) keeps all 16, as TS's
    /// `strncpy(.., 16)` does.
    #[test]
    fn team_names_keep_bytes() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        let cut = b"abcdefghijklmno\xc3";
        let mut msg = vec![3u8];
        msg.extend_from_slice(cut);
        msg.extend_from_slice(b"xyz\0");
        h.apply(Msg::TeamInfo, &msg, &c, &mut fx);
        assert_eq!(shared::text::latin1_bytes(&h.players[3].teamname), cut);
        h.apply(Msg::TeamInfo, b"\x04\xe9quipe\0", &c, &mut fx);
        assert_eq!(shared::text::latin1_bytes(&h.players[4].teamname), b"\xe9quipe");
        h.apply(Msg::TeamNames, b"\x01\xe9quipe\0", &c, &mut fx);
        h.apply(Msg::TeamScore, b"\xe9QUIPE\0\x05\0\0\0", &c, &mut fx);
        // Matched case-insensitively in A-Z only: the score lands on the TeamNames entry.
        assert_eq!(h.team_scores.len(), 1);
        assert_eq!((shared::text::latin1_bytes(&h.team_scores[0].name), h.team_scores[0].frags), (b"\xe9quipe".to_vec(), 5));
        h.apply(Msg::TeamScore, b"\xc9QUIPE\0\x05\0\0\0", &c, &mut fx);
        assert_eq!(h.team_scores.len(), 2, "0xC9 is another team");
    }

    /// `VGUIMenu` hands every id to `ShowVGUIMenu` (which refuses the team menu outside teamplay);
    /// menu 4 brings the map's name, 63 bytes at most.
    #[test]
    fn vgui_menu_message() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        assert!(h.apply(Msg::VguiMenu, &[2], &c, &mut fx));
        let mut four = vec![4u8];
        four.extend_from_slice(&[b'm'; 70]);
        four.push(0);
        h.apply(Msg::VguiMenu, &four, &c, &mut fx);
        assert_eq!(fx, [Effect::VguiMenu(2, None), Effect::VguiMenu(4, Some(vec![b'm'; 63]))]);
    }

    /// `ScoreInfo` stores a negative team number as 0 (`0x100650f6`).
    #[test]
    fn score_info_clamps_the_team() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::ScoreInfo, &ScoreInfo { client: 3, frags: 5, deaths: -2, class: 1, team: -7 }.encode(), &c, &mut fx);
        let p = &h.players[3];
        assert_eq!((p.frags, p.deaths, p.class, p.team), (5, -2, 1, 0));
        h.apply(Msg::ScoreInfo, &ScoreInfo { client: 3, team: 2, ..Default::default() }.encode(), &c, &mut fx);
        assert_eq!(h.players[3].team, 2);
    }

    /// `Geiger` turns the element on with the byte times 4.
    #[test]
    fn geiger_message() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        assert!(!h.geiger_active);
        h.apply(Msg::Geiger, &[100], &c, &mut fx);
        assert_eq!((h.geiger_active, h.geiger_range), (true, 400));
        h.apply(Msg::Geiger, &[0], &c, &mut fx);
        assert_eq!((h.geiger_active, h.geiger_range), (true, 0), "a zero range stays on, silent");
    }

    #[test]
    fn motd_chunks_and_menus() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::Motd, &Motd { last: 0, text: "Hello ".into() }.encode(), &c, &mut fx);
        h.apply(Msg::Motd, &Motd { last: 1, text: "world".into() }.encode(), &c, &mut fx);
        assert_eq!(h.motd, "Hello world");
        assert_eq!(fx, [Effect::OpenMotd]);
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 0b11, time: -1, more: 1, text: "1. A\n".into() }.encode(), &c, &mut fx);
        assert!(h.menu.displayed && h.menu.text.is_empty(), "TS shows the menu after every chunk, with the old text");
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 0b11, time: -1, more: 0, text: "2. #HUD_Credits".into() }.encode(), &c, &mut fx);
        assert_eq!(h.menu.text, "1. A\n2. Credits", "the whole text is localised (BufferedLocaliseTextString)");
        assert_eq!(h.menu_select(3), None);
        assert_eq!(h.menu_select(2).as_deref(), Some("menuselect 2\n"));
        assert!(!h.menu.displayed);
        // A new menu replaces the old text; no slots hides it and still takes the "more" flag.
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 1, time: 5, more: 0, text: "x".repeat(600) }.encode(), &c, &mut fx);
        assert_eq!((h.menu.pending.len(), h.menu.text.len(), h.menu.shutoff), (511, 511, 15.0), "kept to 511 bytes");
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 0, time: 0, more: 1, text: String::new() }.encode(), &c, &mut fx);
        assert!(!h.menu.displayed && h.menu.waiting);
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 1, time: 0, more: 0, text: "y".into() }.encode(), &c, &mut fx);
        assert_eq!(h.menu.text, "x".repeat(511), "waiting: appended to the full old string, and cut again");
    }

    /// `ShowMenu` runs `KB_ConvertString` (`0x10028843`) on the localised text.
    #[test]
    fn show_menu_key_names() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let bind = |b: &[u8]| (b.eq_ignore_ascii_case(b"use")).then(|| b"e".to_vec());
        let c = Ctx { key_binding: &bind, ..ctx(&s) };
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 1, time: 0, more: 0, text: "Press +use or +jump".into() }.encode(), &c, &mut fx);
        assert_eq!(h.menu.text, "Press [e] or +jump");
        h.apply(Msg::ShowMenu, &ShowMenu { slots: 1, time: 0, more: 0, text: "+use".into() }.encode(), &ctx(&s), &mut fx);
        assert_eq!(h.menu.text, "+use", "unbound: as written");
    }

    #[test]
    fn spectator_opens_buy_menu() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::Spectator, &Spectator { client: 1, observer: 1 }.encode(), &c, &mut fx);
        assert!(h.observer);
        assert_eq!(fx, [Effect::OpenBuyMenu, Effect::ResetSpread], "first person stays");
        fx.clear();
        let third = Ctx { third_person: true, ..ctx(&s) };
        h.apply(Msg::Spectator, &Spectator { client: 1, observer: 1 }.encode(), &third, &mut fx);
        assert_eq!(fx, [Effect::ToggleThirdPerson, Effect::OpenBuyMenu, Effect::ResetSpread], "every message, not only on a change");
        fx.clear();
        h.apply(Msg::Spectator, &Spectator { client: 2, observer: 1 }.encode(), &c, &mut fx);
        assert!(fx.is_empty());
        assert!(h.players[2].observer);
        h.apply(Msg::Spectator, &Spectator { client: 1, observer: 2 }.encode(), &c, &mut fx);
        assert!(!h.observer, "only 1 is an observer");
    }

    /// The core asks the ruleset whether to toggle (`ClientRules::spectator_toggles_third_person`,
    /// the stub: whenever in third person); the rulesets' answers are tested in their crates.
    #[test]
    fn spectator_asks_the_ruleset_to_toggle() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let mut h = Hud::default();
        let mut toggles = |third: bool| {
            let c = Ctx { third_person: third, cl_thirdperson: 1.0, ..ctx(&s) };
            let mut fx = Vec::new();
            h.apply(Msg::Spectator, &Spectator { client: 1, observer: 0 }.encode(), &c, &mut fx);
            fx.contains(&Effect::ToggleThirdPerson)
        };
        assert!(toggles(true) && !toggles(false));
    }

    #[test]
    fn thirdperson_switch_on_weapon_info() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let mut h = Hud::default();
        let mut toggles = |old: i32, new: u8, third: bool| {
            h.weapon = old;
            let c = Ctx { third_person: third, thirdperson_switch: true, ..ctx(&s) };
            let mut fx = Vec::new();
            h.apply(Msg::WeaponInfo, &WeaponInfo { id: new, clip: 0, reserve: 0, mode: 0, attachments: 0 }.encode(), &c, &mut fx);
            fx == [Effect::ToggleThirdPerson]
        };
        assert!(toggles(13, 0, false) && toggles(0, 0, false), "kung fu: third person, changed or not");
        assert!(!toggles(13, 0, true));
        assert!(toggles(0, 13, true), "kung fu to a gun: first person");
        assert!(!toggles(13, 13, true) && !toggles(12, 13, true), "not on a gun-to-gun change");
        assert!(!toggles(0, 13, false));
    }

    /// Every handler takes any payload (empty, all 0xff, oversized, random, no NUL) and keeps
    /// its indexed state in range.
    #[test]
    fn handlers_survive_garbage() {
        ts30_gameplay::tables::make_default();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut bufs: Vec<Vec<u8>> = vec![vec![], vec![0xff; 1], vec![0xff; 16], vec![0xff; 700], vec![0; 32], b"%999999999s\0".to_vec()];
        for _ in 0..60 {
            let n = (next() % 48) as usize;
            bufs.push((0..n).map(|_| next() as u8).collect());
        }
        let s = Screen::default();
        let mut h = Hud::default();
        for third in [false, true] {
            let c = Ctx { third_person: third, thirdperson_switch: true, cl_thirdperson: f32::NAN, now: f32::MAX, ..ctx(&s) };
            for b in &bufs {
                for &(m, _, _) in shared::protocol::MESSAGES {
                    let mut fx = Vec::new();
                    h.apply(m, b, &c, &mut fx);
                    // A TextMsg whose format pads to a huge width stays bounded.
                    for f in &fx {
                        if let Effect::ConsolePrint(t) | Effect::CenterPrint(t) | Effect::SayText(t, _) = f {
                            assert!(t.len() < 0x10000);
                        }
                    }
                }
                h.regen_kungfu(crate::gameplay::DEFAULTS.client, f32::INFINITY, f32::NAN, f32::NAN);
                h.tick_round_timer(f32::NAN);
                let _ = h.menu_select(i32::MIN);
                let _ = h.menu_select(10);
            }
        }
        assert_eq!((h.players.len(), h.messages.len(), h.objectives.len()), (MAX_CLIENTS + 1, MAX_TS_MESSAGES, MAX_OBJECTIVES));
        assert!(h.ammo_type(i32::MIN) < 16 && h.ammo_type(i32::MAX) < 16);
        assert!(h.team_names.len() <= MAX_TEAM_NAMES);
        let mut many = vec![9u8];
        for t in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
            many.extend_from_slice(t.as_bytes());
            many.push(0);
        }
        let mut h = Hud::default();
        assert!(h.apply(Msg::TeamNames, &many, &ctx(&s), &mut Vec::new()));
        assert_eq!(h.team_names, ["a", "b", "c", "d"], "TS keeps four");
        let mut h = Hud::default();
        let mut fx = Vec::new();
        h.apply(Msg::TextMsg, &TextMsg { dest: 4, strings: vec!["%999999999s|%-2147483648d|%".into()] }.encode(), &ctx(&s), &mut fx);
        assert_eq!(h.center_text.len(), 2 * 0x400 + 2, "two fields padded to the cap, and two '|'");
    }

    #[test]
    fn round_timer_ticks() {
        ts30_gameplay::tables::make_default();
        let mut h = Hud { round_time: 3.0, ..Default::default() };
        assert_eq!(h.tick_round_timer(1.0), 2);
        assert_eq!(h.tick_round_timer(1.5), 2);
        assert_eq!(h.tick_round_timer(2.1), 1);
        h.round_mode = 2;
        assert_eq!(h.tick_round_timer(5.0), 1, "mode 2 is frozen");
    }

    #[test]
    fn text_msg_formats() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        // `LookupString`: only a whole-string key is looked up; an argument loses one newline.
        let t = TextMsg { dest: 4, strings: vec!["%s has #HUD_Credits".into(), "Bob\n\n".into()] };
        h.apply(Msg::TextMsg, &t.encode(), &c, &mut fx);
        assert_eq!(h.center_text, "Bob\n has #HUD_Credits");
        fx.clear();
        let t = TextMsg { dest: 2, strings: vec!["#HUD_Credits".into(), "#HUD_Credits".into()] };
        h.apply(Msg::TextMsg, &t.encode(), &c, &mut fx);
        assert_eq!(fx, [Effect::ConsolePrint("Credits".into())]);
        // A negative `effect` on the message's entry picks the destination; `\r` becomes `\n`.
        fx.clear();
        let t = TextMsg { dest: 2, strings: vec!["#Notify".into(), "a\rb".into()] };
        h.apply(Msg::TextMsg, &t.encode(), &c, &mut fx);
        assert_eq!(fx, [Effect::ConsolePrint("\u{1}Note a\nb".into())]);
        fx.clear();
        h.apply(Msg::TextMsg, &TextMsg { dest: 9, strings: vec!["x".into()] }.encode(), &c, &mut fx);
        assert!(fx.is_empty(), "other destinations print nothing");
    }

    /// `HudText` reaches `CHudMessage` (a short message is an empty custom message, as TS's
    /// `READ_STRING` gives ""), and `ResetHUD` runs the SDK elements' `Reset`s.
    #[test]
    fn hud_text_and_reset_hud() {
        ts30_gameplay::tables::make_default();
        let s = Screen::default();
        let c = ctx(&s);
        let mut h = Hud::default();
        let mut fx = Vec::new();
        assert!(h.apply(Msg::HudText, b"#Unknown\0", &c, &mut fx));
        assert_eq!(h.message.custom.text, b"#Unknown");
        assert!(h.message.active);
        assert!(h.apply(Msg::HudText, b"", &c, &mut fx));
        assert_eq!(h.message.custom.text, b"", "a short message still adds");
        h.status_bar.msg_status_text(&[0, b'0', b'x', 0]);
        h.sec_ammo.msg_val(&[0, 3]);
        assert!(h.apply(Msg::ResetHUD, &[0], &c, &mut fx));
        assert!(h.message.slots.iter().all(Option::is_none));
        assert!(!h.status_bar.active && h.status_bar.text[0].is_empty());
        assert_eq!(h.sec_ammo.fade, 0.0);
    }
}
