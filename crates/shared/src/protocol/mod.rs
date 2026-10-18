//! User messages, in TS 3.0 registration order with TS sizes (-1 = variable), so the original
//! TS client and this port interoperate. Source: decomp docs/server/architecture.md §3.
//! Payload layouts are documented in architecture.md §3.2 and client/architecture-hud.md §3;
//! per-area encoders/decoders live in submodules. The port's own messages come after TS's 64
//! (docs/implementation/client-plugins.md §12): TS's keep their order and ids.

pub mod effects;
pub mod ext;
pub mod hud;
pub mod hud_wire;
pub mod port;
pub mod rules;
pub mod stunt;
pub mod tspkg;
pub mod weapons;

use std::ffi::CStr;

macro_rules! messages {
    ($($v:ident = $name:literal, $size:expr;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Msg { $($v),* }

        /// (message, wire name, size) in registration order.
        pub const MESSAGES: &[(Msg, &CStr, i32)] = &[$((Msg::$v, $name, $size)),*];
    };
}

messages! {
    SelAmmo = c"SelAmmo", 4;
    CurWeapon = c"CurWeapon", 3;
    Geiger = c"Geiger", 1;
    Health = c"Health", 1;
    Damage = c"Damage", 12;
    Battery = c"Battery", 2;
    Train = c"Train", 1;
    HudText = c"HudText", -1;
    SayText = c"SayText", -1;
    TextMsg = c"TextMsg", -1;
    WeaponList = c"WeaponList", -1;
    ResetHUD = c"ResetHUD", 1;
    InitHUD = c"InitHUD", 0;
    GameTitle = c"GameTitle", 1;
    DeathMsg = c"DeathMsg", -1;
    ScoreInfo = c"ScoreInfo", 9;
    TeamInfo = c"TeamInfo", -1;
    TeamScore = c"TeamScore", -1;
    GameMode = c"GameMode", 1;
    Motd = c"MOTD", -1;
    ServerName = c"ServerName", -1;
    AmmoPickup = c"AmmoPickup", 2;
    WeapPickup = c"WeapPickup", 1;
    ItemPickup = c"ItemPickup", -1;
    HideWeapon = c"HideWeapon", 1;
    SetFov = c"SetFOV", 1;
    ShowMenu = c"ShowMenu", -1;
    ScreenShake = c"ScreenShake", 6;
    ScreenFade = c"ScreenFade", 10;
    TsFade = c"TSFade", -1;
    AmmoX = c"AmmoX", 2;
    TeamNames = c"TeamNames", -1;
    VguiMenu = c"VGUIMenu", 1;
    WeaponInfo = c"WeaponInfo", 6;
    ClipInfo = c"ClipInfo", 1;
    WStatus = c"WStatus", 3;
    ActItems = c"ActItems", 2;
    KFuPower = c"KFuPower", 1;
    TsHealth = c"TSHealth", 1;
    TsArmor = c"TSArmor", 1;
    TsState = c"TSState", 1;
    TsCash = c"TSCash", 2;
    TsSpace = c"TSSpace", 1;
    NextStunt = c"NextStunt", 1;
    PwUp = c"PwUp", 3;
    PwUpTime = c"PwUpTime", 1;
    RoundTime = c"RoundTime", 2;
    Objective = c"Objective", -1;
    ShowObj = c"ShowObj", 2;
    Wires = c"Wires", 3;
    SrvSett = c"SrvSett", 3;
    TsPAward = c"TSPAward", 2;
    TsBTime = c"TSBTime", 1;
    TsHVoice = c"TSHVoice", 1;
    TsFlag = c"TSFlag", 4;
    CustomWp = c"CustomWP", -1;
    Spectator = c"Spectator", 2;
    TsMBlur = c"TSMBlur", 2;
    TsMessage = c"TSMessage", -1;
    TsSlowMo = c"TSSlowMo", 2;
    PTakeDam = c"PTakeDam", 13;
    TsFrags = c"TSFrags", -1;
    PlInfo = c"PlInfo", 3;
    TsPSys = c"TSPSys", -1;
    // ---- port additions, after TS's 64 ----
    // Server-to-client-extension traffic (`ext`, issue #294). Sent only to port clients.
    TsExt = c"TSExt", -1;
    // The plugin offers and version notices (`tspkg`, issue #298). Sent only to port clients
    // that hook it (from `port::TSPKG_SINCE`).
    TsPkg = c"TSPkg", -1;
}

/// How many of [`MESSAGES`] are TS 3.0's own: the port's come after them.
pub const TS_MESSAGES: usize = 64;

impl Msg {
    pub fn name(self) -> &'static CStr {
        MESSAGES[self as usize].1
    }
    pub fn size(self) -> i32 {
        MESSAGES[self as usize].2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        for (i, (m, name, _)) in MESSAGES.iter().enumerate() {
            assert_eq!(*m as usize, i);
            assert!(name.to_bytes().len() <= 11, "{name:?} too long for the engine");
        }
        assert_eq!(Msg::TsSlowMo.name(), c"TSSlowMo");
        assert_eq!(Msg::TsPSys as usize, TS_MESSAGES - 1, "TS's messages keep their order");
        assert_eq!(Msg::TsExt as usize, TS_MESSAGES);
        assert_eq!(Msg::TsPkg as usize, TS_MESSAGES + 1);
        assert_eq!(Msg::TsPkg.name(), tspkg::NAME);
    }
}

/// The server encodes HUD messages with `hud_wire` (rules package) and the client decodes them
/// with `hud` (HUD package). These tests pin the two to the same wire layout.
#[cfg(test)]
mod wire_agreement {
    use super::{hud, hud_wire as w};

    fn enc(f: impl Fn(&mut w::Bytes)) -> Vec<u8> {
        let mut b = w::Bytes::default();
        f(&mut b);
        b.buf
    }

    #[test]
    fn server_encoders_match_client_decoders() {
        let b = enc(|b| w::TsHealth { hp: 87 }.write(b));
        assert_eq!(hud::TsHealth::decode(&b).unwrap().hp, 87);
        let b = enc(|b| w::TsArmor { armor: 100 }.write(b));
        assert_eq!(hud::TsArmor::decode(&b).unwrap().armor, 100);
        let b = enc(|b| w::TsState { state: 2 }.write(b));
        assert_eq!(hud::TsState::decode(&b).unwrap().stance, 2);
        let b = enc(|b| w::TsCash { cash: 12345 }.write(b));
        assert_eq!(hud::TsCash::decode(&b).unwrap().cash, 12345);
        let b = enc(|b| w::TsSpace { space: 41 }.write(b));
        assert_eq!(hud::TsSpace::decode(&b).unwrap().slots, 41);
        let b = enc(|b| w::PwUp { bits: 0x108, secs: 9 }.write(b));
        let d = hud::PwUp::decode(&b).unwrap();
        assert_eq!((d.flags, d.value), (0x108, 9));
        let b = enc(|b| w::PwUpTime { secs: 7 }.write(b));
        assert_eq!(hud::PwUpTime::decode(&b).unwrap().secs, 7);
        let b = enc(|b| w::TsSlowMo { factor_x100: 35.0 }.write(b));
        assert_eq!(hud::TsSlowMo::decode(&b).unwrap().factor100, 35.0);
        let b = enc(|b| w::TsBTime { on: 1 }.write(b));
        assert_eq!(hud::TsBTime::decode(&b).unwrap().on, 1);
        let b = enc(|b| w::TsMBlur { entindex: 5, mode: 2 }.write(b));
        let d = hud::TsMBlur::decode(&b).unwrap();
        assert_eq!((d.entity, d.mode), (5, 2));
        let b = enc(|b| w::TsPAward { entindex: 3, award: 4 }.write(b));
        let d = hud::TsPAward::decode(&b).unwrap();
        assert_eq!((d.client, d.award), (3, 4));
        let b = enc(|b| w::PTakeDam { x: 1.0, y: -2.5, z: 3.0, dx: 0.5, dy: 0.0, dz: -1.0, amount: 130 }.write(b));
        let d = hud::PTakeDam::decode(&b).unwrap();
        assert_eq!((d.px, d.py, d.pz, d.dx, d.dz, d.amount), (1.0, -2.5, 3.0, 0.5, -1.0, 130));
        let b = enc(|b| w::TsFade { duration: 4096, hold: 2048, flags: 1, r: 255, g: 0, b: 0, a: 128 }.write(b));
        let d = hud::TsFade::decode(&b).unwrap();
        assert_eq!((d.duration, d.hold, d.flags, d.r, d.a), (4096, 2048, 1, 255, 128));
        let b = enc(|b| w::Train { pos: 5 }.write(b));
        assert_eq!(hud::Train::decode(&b).unwrap().pos, 5);
        let b = enc(|b| w::HideWeapon { flags: 1 }.write(b));
        assert_eq!(hud::HideWeapon::decode(&b).unwrap().flags, 1);
    }
}
