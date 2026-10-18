//! SDK elements: `CHudDeathNotice` as TS changed it (the weapon drawn as text, the `nohud`
//! gate), `GetClientColor`, and `CHudMenu` (ShowMenu).

use super::Dc;
use super::draw::{Canvas, EngineCanvas, HudSprites, ftol};
use super::saytext;
use super::state::PlayerExtra;
use goldsrc::client::engine;

/// `MAX_DEATHNOTICES`.
pub const MAX_DEATHNOTICES: usize = 4;
/// `MAX_PLAYER_NAME_LENGTH`: the name fields hold 31 bytes.
const NAME_LENGTH: usize = 32;

/// `GetClientColor` (`0x1001af40`): white for the local player, else the colour of teams 1-4
/// (`g_PlayerExtraInfo[cl].teamnumber`), `None` (a null pointer: the engine's current text
/// colour) for any other team.
pub fn client_color(players: &[PlayerExtra], local: i32, cl: i32) -> Option<[f32; 3]> {
    if cl == local {
        return Some([1.0, 1.0, 1.0]);
    }
    // Guarded: TS indexes `g_PlayerExtraInfo` with any client number; past the table is team 0.
    match usize::try_from(cl).ok().and_then(|i| players.get(i)).map_or(0, |p| p.team) {
        1 => Some([0.6, 0.8, 1.0]),
        2 => Some([1.0, 0.25, 0.25]),
        3 => Some(saytext::SAY_COLOR),
        4 => Some([0.6, 1.0, 0.6]),
        _ => None,
    }
}

/// `g_PlayerInfoList[i].name` after `GetPlayerInfo(i)`: `None` for a null (or empty) name.
/// Guarded: an index outside `0..=max` (past TS's table, which it reads or writes anyway) has
/// no name.
pub fn player_name(i: i32, max: i32) -> Option<Vec<u8>> {
    if !(0..=max).contains(&i) {
        return None;
    }
    let n = engine().player_info(i).name_bytes;
    (!n.is_empty()).then_some(n)
}

/// A `DeathNoticeItem` (0xbc bytes, list at `0x100d5cc8`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeathNotice {
    /// `szKiller` (`+0x00`).
    pub killer: Vec<u8>,
    /// `szVictim` (`+0x40`): a name, or the object for a non-player kill.
    pub victim: Vec<u8>,
    /// `iId` (`+0x80`): TS always stores -1, the `d_skull` sprite.
    pub id: i32,
    /// `iSuicide` (`+0x84`).
    pub suicide: bool,
    /// `iTeamKill` (`+0x88`).
    pub team_kill: bool,
    /// `iNonPlayerKill` (`+0x8c`).
    pub non_player_kill: bool,
    /// `flDisplayTime` (`+0x90`).
    pub display_time: f32,
    /// `KillerColor` (`+0x94`), `None` = null.
    pub killer_color: Option<[f32; 3]>,
    /// `VictimColor` (`+0x98`), `None` = null.
    pub victim_color: Option<[f32; 3]>,
    /// TS's addition (`+0x9c`, 32 bytes): the weapon name, drawn as text in place of a sprite.
    /// Only a plain kill keeps it; suicides, team kills and non-player kills show `d_skull`.
    pub weapon: Vec<u8>,
}

/// `CHudDeathNotice` (element `+0x570` = `0x100cfe90`).
#[derive(Debug, Clone)]
pub struct DeathNotices {
    /// `rgDeathNoticeList`, the live entries (TS's free slots are always zeroed and at the end).
    pub list: Vec<DeathNotice>,
    /// `DEATHNOTICE_DISPLAY_TIME` (`0x10093574`): an int, initially 6, set from
    /// `hud_deathnotice_time` by each `DeathMsg`.
    pub display_time: i32,
}

impl Default for DeathNotices {
    fn default() -> Self {
        DeathNotices { list: Vec::new(), display_time: 6 }
    }
}

fn name_field(n: &[u8]) -> Vec<u8> {
    n[..n.len().min(NAME_LENGTH - 1)].to_vec()
}

/// What `MsgFunc_DeathMsg` reads from outside the element.
pub struct DeathEnv<'a> {
    /// `g_PlayerInfoList[i].name`.
    pub name: &'a dyn Fn(u8) -> Option<Vec<u8>>,
    /// `GetClientColor`.
    pub color: &'a dyn Fn(u8) -> Option<[f32; 3]>,
    /// `gHUD.m_flTime`.
    pub now: f32,
    /// `hud_deathnotice_time`.
    pub cvar_time: f32,
}

impl DeathNotices {
    /// `InitHUDData` (`0x1001afe0`): the list emptied.
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// `MsgFunc_DeathMsg` (`0x1001b360`) from the list on: a new entry (the oldest dropped when
    /// all four are in use) and the console line, which is returned.
    ///
    /// Guarded: TS builds `"d_" + weapon` with `strncat(.., 32)` in a 32-byte buffer, so a
    /// weapon name over 29 bytes runs over it; the port cuts it at 29.
    pub fn add(&mut self, killer: u8, victim: u8, weapon: &[u8], env: &DeathEnv) -> Vec<u8> {
        let mut killed_with = b"d_".to_vec();
        killed_with.extend_from_slice(&weapon[..weapon.len().min(29)]);
        if self.list.len() == MAX_DEATHNOTICES {
            self.list.remove(0);
        }
        let mut d = DeathNotice::default();
        if let Some(n) = (env.name)(killer) {
            d.killer_color = (env.color)(killer);
            d.killer = name_field(&n);
        }
        // Victim -1: the killer destroyed a non-player object.
        let object = victim == 0xff;
        if let Some(n) = if object { None } else { (env.name)(victim) } {
            d.victim_color = (env.color)(victim);
            d.victim = name_field(&n);
        }
        if object {
            d.non_player_kill = true;
            d.victim = killed_with[2..].to_vec();
        } else {
            d.suicide = killer == victim || killer == 0;
            d.team_kill = killed_with == b"d_teammate";
        }
        d.id = -1;
        if killed_with.len() > 2 {
            d.weapon = killed_with[2..].to_vec();
        }
        self.display_time = ftol(env.cvar_time as f64);
        d.display_time = (self.display_time as f64 + env.now as f64) as f32;

        let mut line = Vec::new();
        if d.non_player_kill {
            line.extend_from_slice(&d.killer);
            line.extend_from_slice(b" killed a ");
            line.extend_from_slice(&d.victim);
            line.push(b'\n');
            d.weapon.clear();
            self.list.push(d);
            return line;
        }
        if d.suicide {
            line.extend_from_slice(&d.victim);
            line.extend_from_slice(if killed_with == b"d_world" { b" died" } else { b" killed self" });
            d.weapon.clear();
        } else if d.team_kill {
            line.extend_from_slice(&d.killer);
            line.extend_from_slice(b" killed his teammate ");
            line.extend_from_slice(&d.victim);
            d.weapon.clear();
        } else {
            line.extend_from_slice(&d.killer);
            line.extend_from_slice(b" killed ");
            line.extend_from_slice(&d.victim);
        }
        // TS also tests `*killedwith > 13`, always true ('d').
        if killed_with != b"d_world" && !d.team_kill {
            line.extend_from_slice(b" with ");
            let w: &[u8] = &killed_with[2..];
            line.extend_from_slice(match w {
                b"egon" => b"gluon gun",
                b"gauss" => b"tau cannon",
                _ => w,
            });
        }
        line.push(b'\n');
        self.list.push(d);
        line
    }

    /// `Draw` (`0x1001b020`, skipped under `nohud`): expired entries are dropped and the rest
    /// clamped to `DEATHNOTICE_DISPLAY_TIME` from now; while `AllowedToPrintText`, each is drawn
    /// right aligned, 20 px apart from y 20: the killer (unless a suicide), then the weapon name
    /// in (1, 0.5, 0.2) or, without one, `d_skull` in white (green for a team kill), then the
    /// victim (unless a non-player kill).
    pub fn draw(&mut self, now: f32, allowed: bool, screen_width: i32, sprites: &HudSprites, c: &mut dyn Canvas) {
        let skull = sprites.index("d_skull");
        let mut i = 0;
        while i < self.list.len() {
            let limit = self.display_time as f64 + now as f64;
            let d = &mut self.list[i];
            if d.display_time < now {
                self.list.remove(i);
                continue;
            }
            if (d.display_time as f64) >= limit {
                d.display_time = limit as f32;
            }
            if allowed {
                draw_notice(d, 20 + 20 * i as i32, screen_width, skull, sprites, c);
            }
            i += 1;
        }
    }
}

fn draw_notice(d: &DeathNotice, y: i32, screen_width: i32, skull: Option<usize>, sprites: &HudSprites, c: &mut dyn Canvas) {
    // Guarded: with no `d_skull` in hud.txt TS uses index -1 and reads a rectangle from before
    // the table; the port gives it no width and draws no sprite.
    let rect = skull.and_then(|i| sprites.rects.get(i)).copied().unwrap_or_default();
    // C int arithmetic: the rectangle is hud.txt data and the widths the engine's.
    let sprite_width = rect.right.wrapping_sub(rect.left);
    let victim_width = c.console_size(&d.victim).0;
    let mut x = if d.weapon.is_empty() {
        screen_width.wrapping_sub(sprite_width).wrapping_sub(victim_width).wrapping_sub(5)
    } else {
        screen_width.wrapping_sub(c.console_size(&d.weapon).0).wrapping_sub(victim_width).wrapping_sub(10)
    };
    if !d.suicide {
        x = x.wrapping_sub(c.console_size(&d.killer).0.wrapping_add(5));
        x = c.console_string(x, y, d.killer_color, &d.killer).wrapping_add(5);
    }
    if d.weapon.is_empty() {
        let rgb = if d.team_kill { [10, 240, 10] } else { [255, 255, 255] };
        if let Some(i) = skull {
            c.sprite(i, rgb, x, y);
        }
        x = x.wrapping_add(sprite_width);
    } else {
        x = c.console_string(x, y, Some([1.0, 0.5, 0.2]), &d.weapon).wrapping_add(5);
    }
    if !d.non_player_kill {
        c.console_string(x, y, d.victim_color, &d.victim);
    }
}

/// `MsgFunc_DeathMsg` (`0x1001b360`): byte killer, byte victim, string weapon. After the
/// scoreboard's `DeathMsg`, `CHudSpectator::DeathMessage(victim)` (the overview's dead icon),
/// then the list entry and the console line. A short message is ignored (see Deviations).
///
/// Split in two for the client extensions (issue #295): the spectator's notice is state, which
/// runs when the message comes ([`on_death_msg_state`]); the kill feed and its console line are
/// display ([`on_death_msg_display`]), which an extension can block, rewrite or hold.
pub fn on_death_msg_state(buf: &[u8]) {
    let Some(m) = shared::protocol::hud::DeathMsg::decode(buf) else { return };
    crate::spectator::death_notice(m.victim as i32);
}

/// The kill feed's line and the console's ([`on_death_msg_state`]).
pub fn on_death_msg_display(buf: &[u8]) {
    let Some(m) = shared::protocol::hud::DeathMsg::decode(buf) else { return };
    let local = super::local_index();
    // `GetAllPlayersInfo` (`0x10064880`) refreshes players 1..63 just before the names are read;
    // slot 0 is never filled.
    let name = |i: u8| if i == 0 { None } else { player_name(i as i32, 63) };
    let weapon = saytext::bytes(&m.weapon);
    let (now, cvar_time) = (super::now(), super::cvars().deathnotice_time.value());
    let line = super::with(|h| {
        let players = &h.players;
        let color = |i: u8| client_color(players, local, i as i32);
        let env = DeathEnv { name: &name, color: &color, now, cvar_time };
        h.deaths.add(m.killer, m.victim, &weapon, &env)
    });
    engine().print_bytes(&line);
}

/// The death notices' turn in `CHud::Redraw` (the caller checks `nohud`).
pub fn draw_death_notices(dc: &Dc) {
    let allowed = crate::menus::allowed_to_print_text();
    let mut canvas = EngineCanvas(&dc.g.sprites);
    super::with(|h| h.deaths.draw(dc.now, allowed, dc.w(), &dc.g.sprites, &mut canvas));
}

/// `g_szMenuString`'s 512 bytes: `CHudMenu::Draw` scans no further.
const MENU_STRING_LEN: usize = 0x200;

/// `CHudMenu::Draw`'s layout (`0x100285f0`): the start of each line and its y. TS counts the
/// `\n`s (up to the NUL or 512 bytes), starts at `h/2 - (lines/2)·12 - 40` and steps 12 px per
/// line, whatever the font. Each line is drawn whole by `DrawHudString`, so a byte past 512 still
/// shows if its line started before. There are no colour or alignment escapes: TS's `CHudMenu`
/// predates Half-Life's `\w \d \y \r \R`, which it would draw as text.
pub fn menu_lines(text: &[u8], screen_height: i32) -> Vec<(usize, i32)> {
    let at = |i: usize| text.get(i).copied().unwrap_or(0);
    let lines = (0..MENU_STRING_LEN).take_while(|&i| at(i) != 0).filter(|&i| at(i) == b'\n').count() as i32;
    let mut y = screen_height / 2 - (lines / 2) * 12 - 40;
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        if at(i) == 0 {
            return out;
        }
        out.push((i, y));
        y += 12;
        while i < MENU_STRING_LEN && at(i) != 0 && at(i) != b'\n' {
            i += 1;
        }
        if at(i) == b'\n' {
            i += 1;
        }
        if i >= MENU_STRING_LEN {
            return out;
        }
    }
}

/// `CHudMenu::Draw` (`0x100285f0`): the menu expires at its shutoff time, hides under the
/// scoreboard, and otherwise draws each line in white at x 20, clipped at x 320
/// (`DrawHudString(20, y, 320, line, 255, 255, 255)`).
pub fn draw_menu(dc: &Dc) {
    let menu = super::with(|h| {
        if h.menu.displayed && h.menu.shutoff > 0.0 && h.menu.shutoff <= dc.now {
            h.menu.displayed = false;
        }
        h.menu.displayed.then(|| h.menu.text.clone())
    });
    let Some(text) = menu else { return };
    if crate::menus::scores_visible() {
        return;
    }
    let bytes = saytext::bytes(&text);
    for (start, y) in menu_lines(&bytes, dc.h()) {
        let line: String = text.chars().skip(start).collect();
        super::text::draw_string(&dc.g.screen, 20, y, 320, &line, [255, 255, 255]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc::sys::wrect_t;

    #[test]
    fn menu_lines_follow_ts() {
        // 480 px, three lines (two `\n`s): 240 - 1·12 - 40 = 188, then 12 px steps.
        let t = b"\\yTitle\n1. A\n2. B";
        assert_eq!(menu_lines(t, 480), vec![(0, 188), (8, 200), (13, 212)], "escapes are plain text");
        assert_eq!(menu_lines(b"", 480), vec![]);
        assert_eq!(menu_lines(b"a\n", 600), vec![(0, 260)], "a trailing newline starts no line");
        // Only the first 512 bytes are scanned, for the count and for the line starts.
        let mut long = vec![b'x'; 600];
        long[100] = b'\n';
        long[550] = b'\n';
        assert_eq!(menu_lines(&long, 480), vec![(0, 200), (101, 212)]);
    }

    fn names(i: u8) -> Option<Vec<u8>> {
        match i {
            1 => Some(b"Alice".to_vec()),
            2 => Some(b"Bob".to_vec()),
            3 => Some(b"a name that is longer than thirty-one bytes".to_vec()),
            _ => None,
        }
    }

    fn colors(i: u8) -> Option<[f32; 3]> {
        (i == 1).then_some([1.0, 1.0, 1.0])
    }

    fn add(n: &mut DeathNotices, killer: u8, victim: u8, weapon: &str, now: f32) -> String {
        let env = DeathEnv { name: &names, color: &colors, now, cvar_time: 6.9 };
        String::from_utf8(n.add(killer, victim, weapon.as_bytes(), &env)).unwrap()
    }

    #[test]
    fn console_lines() {
        let mut n = DeathNotices::default();
        assert_eq!(add(&mut n, 1, 2, "glock18", 0.0), "Alice killed Bob with glock18\n");
        assert_eq!(add(&mut n, 2, 2, "grenade", 0.0), "Bob killed self with grenade\n");
        assert_eq!(add(&mut n, 0, 2, "world", 0.0), "Bob died\n");
        assert_eq!(add(&mut n, 1, 2, "teammate", 0.0), "Alice killed his teammate Bob\n");
        assert_eq!(add(&mut n, 1, 0xff, "func_breakable", 0.0), "Alice killed a func_breakable\n");
        assert_eq!(add(&mut n, 1, 2, "egon", 0.0), "Alice killed Bob with gluon gun\n");
        assert_eq!(add(&mut n, 1, 2, "gauss", 0.0), "Alice killed Bob with tau cannon\n");
        assert_eq!(add(&mut n, 1, 2, "", 0.0), "Alice killed Bob with \n");
        assert_eq!(add(&mut n, 9, 2, "uzi", 0.0), " killed Bob with uzi\n", "a missing name is empty");
    }

    #[test]
    fn entries_follow_ts() {
        let mut n = DeathNotices::default();
        add(&mut n, 1, 2, "glock18", 10.0);
        let d = &n.list[0];
        assert_eq!((d.id, d.weapon.as_slice(), d.suicide, d.team_kill), (-1, &b"glock18"[..], false, false));
        assert_eq!((d.killer_color, d.victim_color), (Some([1.0; 3]), None));
        assert_eq!(n.display_time, 6, "the cvar truncated to an int");
        assert_eq!(d.display_time, 16.0);
        add(&mut n, 2, 2, "grenade", 10.0);
        assert!(n.list[1].suicide && n.list[1].weapon.is_empty(), "suicides show the skull");
        add(&mut n, 1, 2, "teammate", 10.0);
        assert!(n.list[2].team_kill && n.list[2].weapon.is_empty());
        add(&mut n, 3, 0xff, "func_breakable", 10.0);
        let d = &n.list[3];
        assert!(d.non_player_kill && d.weapon.is_empty());
        assert_eq!(d.killer.len(), 31, "names keep 31 bytes");
        assert_eq!(d.victim, b"func_breakable");
        add(&mut n, 1, 2, "uzi", 10.0);
        assert_eq!(n.list.len(), MAX_DEATHNOTICES);
        assert!(n.list[0].suicide, "the oldest (the glock18 kill) went");
        assert_eq!(n.list[3].weapon, b"uzi");
        add(&mut n, 1, 2, &"w".repeat(40), 10.0);
        assert_eq!(n.list[3].weapon.len(), 29, "guarded at the 32-byte buffer");
    }

    #[derive(Default)]
    struct Rec(Vec<String>);
    impl Canvas for Rec {
        fn console_size(&self, t: &[u8]) -> (i32, i32) {
            (t.len() as i32 * 10, 12)
        }
        fn console_string(&mut self, x: i32, y: i32, c: Option<[f32; 3]>, t: &[u8]) -> i32 {
            self.0.push(format!("text {x},{y} {c:?} {}", String::from_utf8_lossy(t)));
            x + t.len() as i32 * 10
        }
        fn character(&mut self, _: i32, _: i32, _: u8, _: [i32; 3]) {}
        fn sprite(&mut self, i: usize, rgb: [i32; 3], x: i32, y: i32) {
            self.0.push(format!("sprite {i} {rgb:?} {x},{y}"));
        }
    }

    #[test]
    fn draw_layout_and_expiry() {
        let sprites = HudSprites {
            names: vec!["d_skull".into()],
            handles: vec![7],
            rects: vec![wrect_t { left: 0, right: 32, top: 0, bottom: 16 }],
            number_0: None,
        };
        let mut n = DeathNotices::default();
        add(&mut n, 1, 2, "ak", 0.0);
        add(&mut n, 2, 2, "grenade", 0.0);
        add(&mut n, 1, 2, "teammate", 0.0);
        let mut r = Rec::default();
        n.draw(1.0, true, 640, &sprites, &mut r);
        let white = Some([1.0f32; 3]);
        assert_eq!(
            r.0,
            [
                // 640 - "ak" 20 - "Bob" 30 - 10 = 580; killer "Alice" 50 + 5 before it.
                format!("text 525,20 {white:?} Alice"),
                "text 580,20 Some([1.0, 0.5, 0.2]) ak".into(),
                "text 605,20 None Bob".into(),
                // Suicide: 640 - 32 - 30 - 5 = 573, no killer, white skull.
                "sprite 0 [255, 255, 255] 573,40".into(),
                "text 605,40 None Bob".into(),
                // Team kill: green skull.
                format!("text 518,60 {white:?} Alice"),
                "sprite 0 [10, 240, 10] 573,60".into(),
                "text 605,60 None Bob".into(),
            ]
        );
        // Not allowed to print: nothing drawn, but the list still ages.
        let mut r = Rec::default();
        n.draw(6.5, false, 640, &sprites, &mut r);
        assert!(r.0.is_empty());
        assert_eq!(n.list.len(), 0, "display time 6 passed");
    }

    /// Any killer/victim bytes, long names and weapons, a wild `hud_deathnotice_time`, and a
    /// hud.txt `d_skull` rectangle at the ends of the int range.
    #[test]
    fn hostile_notices_and_rects() {
        struct Huge;
        impl Canvas for Huge {
            fn console_size(&self, _: &[u8]) -> (i32, i32) {
                (i32::MAX, i32::MAX)
            }
            fn console_string(&mut self, _: i32, _: i32, _: Option<[f32; 3]>, _: &[u8]) -> i32 {
                i32::MAX
            }
            fn character(&mut self, _: i32, _: i32, _: u8, _: [i32; 3]) {}
            fn sprite(&mut self, _: usize, _: [i32; 3], _: i32, _: i32) {}
        }
        let long: &dyn Fn(u8) -> Option<Vec<u8>> = &|_| Some(vec![0xff; 500]);
        let none: &dyn Fn(u8) -> Option<Vec<u8>> = &|_| None;
        let mut n = DeathNotices::default();
        for (i, t) in [f32::NAN, f32::INFINITY, -1e30, 6.0].into_iter().enumerate() {
            for (k, v) in [(0u8, 0u8), (0xff, 0xff), (64, 65), (1, 0xff)] {
                let env = DeathEnv { name: if i % 2 == 0 { long } else { none }, color: &colors, now: f32::MAX, cvar_time: t };
                n.add(k, v, &[b'w'; 300], &env);
                n.add(k, v, b"", &env);
                assert!(n.list.len() <= MAX_DEATHNOTICES);
            }
        }
        let sprites = HudSprites {
            names: vec!["d_skull".into()],
            handles: vec![],
            rects: vec![wrect_t { left: i32::MIN, right: i32::MAX, top: 0, bottom: 0 }],
            number_0: None,
        };
        for w in [i32::MIN, 0, i32::MAX] {
            n.draw(0.0, true, w, &sprites, &mut Huge);
        }
        assert_eq!(client_color(&[], 3, -1), None);
        assert_eq!(client_color(&[], 3, i32::MAX), None);
        assert_eq!(menu_lines(b"\n\n\n", i32::MIN).len(), 3);
    }

    #[test]
    fn display_time_is_clamped() {
        let mut n = DeathNotices::default();
        add(&mut n, 1, 2, "ak", 100.0); // display 106
        n.display_time = 2;
        n.draw(101.0, false, 640, &HudSprites::default(), &mut Rec::default());
        assert_eq!(n.list[0].display_time, 103.0, "min(display, now + DEATHNOTICE_DISPLAY_TIME)");
    }
}
