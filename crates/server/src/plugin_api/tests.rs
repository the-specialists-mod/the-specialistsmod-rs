use super::*;
use shared::ts::powerup as pw;
use std::cell::Cell;

/// A fake game: player 1 is connected (user id 7); `reachable` says whether its object can be
/// reached now, `refuse` whether a change turns out impossible.
struct Fake {
    reachable: bool,
    refuse: bool,
    state: PlayerInfo,
    applied: Cell<Option<Change>>,
}

impl Fake {
    fn new(reachable: bool) -> Self {
        Fake { reachable, refuse: false, state: PlayerInfo { cash: 500, ..PlayerInfo::default() }, applied: Cell::new(None) }
    }
}

impl Host for Fake {
    fn user_id(&self, player: c_int) -> Option<i32> {
        (player == 1).then_some(7)
    }
    fn read(&self, _: c_int) -> Option<PlayerInfo> {
        self.reachable.then_some(self.state)
    }
    fn bare(&self, _: c_int) -> PlayerInfo {
        PlayerInfo { time_scale: 0.5, ..PlayerInfo::default() }
    }
    fn apply(&self, _: c_int, c: Change) -> Reach {
        self.applied.set(Some(c));
        match (self.reachable, self.refuse) {
            (false, _) => Reach::Later,
            (true, true) => Reach::Refused,
            (true, false) => Reach::Now,
        }
    }
}

#[test]
fn the_interface_name_is_the_tables() {
    assert!(interface(c"TSRS_PluginApi008"));
    assert!(!interface(c"TSRS_PluginApi007"));
    assert!(!interface(c"TSRS_PluginApi006"));
    // Each version breaks the table: plugins built for an older one ask for its name and get NULL.
    assert!(!interface(c"TSRS_PluginApi005"));
    assert!(!interface(c"TSRS_PluginApi004"));
    assert!(!interface(c"TSRS_PluginApi003"));
    assert!(!interface(c"TSRS_PluginApi001"));
    assert!(!interface(c"TSRS_PluginApi002"));
    assert!(!interface(c"VEngineServer021"));
}

#[test]
fn setters_check_the_player_then_the_arguments() {
    let host = Fake::new(true);
    assert_eq!(set(&host, 2, Change::cash(100)), NOT_PLAYER, "no player in slot 2");
    assert_eq!(set(&host, 0, Change::cash(-1)), NOT_PLAYER, "a bad player wins over bad arguments");
    assert_eq!(host.applied.get(), None);
    assert_eq!(set(&host, 1, Change::cash(-1)), INVALID);
    assert_eq!(host.applied.get(), None, "nothing changes on INVALID");
    assert_eq!(set(&host, 1, Change::cash(100)), OK);
    assert_eq!(host.applied.get(), Some(Change::Cash(100)));
}

#[test]
fn a_taken_out_player_defers_and_a_refused_change_is_invalid() {
    ts30_gameplay::tables::make_default();
    let host = Fake::new(false);
    assert_eq!(set(&host, 1, Change::award(4)), DEFERRED);
    assert_eq!(host.applied.get(), Some(Change::Award(4)));
    assert_eq!(set(&host, 1, Change::award(300)), INVALID, "arguments are checked before deferring");
    let host = Fake { refuse: true, ..Fake::new(true) };
    assert_eq!(set(&host, 1, Change::weapon_ammo(5, 10, -1)), INVALID, "a weapon the player doesn't own");
}

#[test]
fn player_info_falls_back_to_the_snapshot() {
    let snaps = RefCell::new(Snapshots::default());
    assert_eq!(info(&Fake::new(true), &snaps, 3), Err(NOT_PLAYER));
    // Out of reach with no snapshot: what the engine knows.
    assert_eq!(info(&Fake::new(false), &snaps, 1).unwrap().time_scale, 0.5);
    // Read live: stored.
    let live = info(&Fake::new(true), &snaps, 1).unwrap();
    assert_eq!(live.cash, 500);
    // Out of reach: the snapshot.
    let mut out = Fake::new(false);
    out.state.cash = 9;
    assert_eq!(info(&out, &snaps, 1).unwrap(), live);
}

#[test]
fn snapshots_belong_to_their_client() {
    let mut s = Snapshots::default();
    let a = PlayerInfo { cash: 1, ..PlayerInfo::default() };
    s.put(4, 10, a);
    assert_eq!(s.get(4, 10), Some(a));
    assert_eq!(s.get(4, 11), None, "the slot's next client");
    assert_eq!(s.get(5, 10), None);
    assert_eq!(s.get(-1, 10), None);
    s.put(-1, 10, a); // ignored
}

#[test]
fn argument_ranges() {
    ts30_gameplay::tables::make_default();
    assert_eq!(Change::cash(30000), Ok(Change::Cash(30000)));
    assert_eq!(Change::cash(30001), Err(INVALID));
    assert_eq!(Change::free_slots(0), Ok(Change::FreeSlots(0)));
    assert_eq!(Change::free_slots(81), Ok(Change::FreeSlots(81)));
    assert_eq!(Change::free_slots(82), Err(INVALID));
    assert_eq!(Change::award(255), Ok(Change::Award(255)));
    assert_eq!(Change::award(-1), Err(INVALID));

    assert_eq!(Change::speed(0.0, 0.0, 5.0), Ok(Change::Speed { amount: 0.0, aura: 0.0, secs: 5.0 }));
    assert_eq!(Change::speed(2.5, 100.0, 5.0), Err(INVALID));
    assert_eq!(Change::speed(0.5, -1.0, 5.0), Err(INVALID));
    assert_eq!(Change::speed(0.5, 100.0, -1.0), Err(INVALID));
    assert_eq!(Change::speed(f32::NAN, 100.0, 1.0), Err(INVALID));
    assert_eq!(Change::speed(0.5, f32::INFINITY, 1.0), Err(INVALID));

    assert_eq!(Change::physics_speed(1.0), Ok(Change::PhysicsSpeed(None)), "1 hands it back to TS");
    assert_eq!(Change::physics_speed(-3.0), Ok(Change::PhysicsSpeed(Some(0.01))), "negative pauses");
    assert_eq!(Change::physics_speed(0.5), Ok(Change::PhysicsSpeed(Some(0.5))));
    assert_eq!(Change::physics_speed(2.1), Err(INVALID));
    assert_eq!(Change::physics_speed(f32::NAN), Err(INVALID));

    assert_eq!(Change::weapon_ammo(5, 30, -1), Ok(Change::WeaponAmmo { id: 5, clip: Some(30), ammo: None }));
    assert_eq!(Change::weapon_ammo(5, -1, 90), Ok(Change::WeaponAmmo { id: 5, clip: None, ammo: Some(90) }));
    assert_eq!(Change::weapon_ammo(0, 1, 1), Err(INVALID), "kung fu has no ammo");
    assert_eq!(Change::weapon_ammo(29, 1, 1), Err(INVALID), "no weapon 29 in TS 3.0");
    assert_eq!(Change::weapon_ammo(300, 1, 1), Err(INVALID));
    assert_eq!(Change::weapon_ammo(5, 256, 1), Err(INVALID));
    assert_eq!(Change::weapon_ammo(5, 1, 32768), Err(INVALID));
}

#[test]
fn powerups_that_run() {
    let slowmo = pw::SLOWMO as c_int;
    assert_eq!(Change::powerup(0, 123.0), Ok(Change::Powerup { bits: 0, secs: 0.0 }), "0 stops");
    assert_eq!(Change::powerup(slowmo, 10.0), Ok(Change::Powerup { bits: pw::SLOWMO, secs: 10.0 }));
    // `SetupPowerUp`'s base durations with `pwupmult` 1.
    assert_eq!(Change::powerup(slowmo, -1.0), Ok(Change::Powerup { bits: pw::SLOWMO, secs: 3.0 }));
    assert_eq!(Change::powerup(pw::SLOWPAUSE as c_int, -1.0), Ok(Change::Powerup { bits: pw::SLOWPAUSE, secs: 2.0 }));
    assert_eq!(Change::powerup(pw::DOUBLE_FIRERATE as c_int, -5.0), Ok(Change::Powerup { bits: pw::DOUBLE_FIRERATE, secs: 3.0 }));
    assert_eq!(Change::powerup(pw::INFINITE_CLIP as c_int, -5.0), Ok(Change::Powerup { bits: pw::INFINITE_CLIP, secs: 3.0 }));
    assert_eq!(Change::powerup(pw::SUPERJUMP as c_int, 4.0), Ok(Change::Powerup { bits: pw::SUPERJUMP, secs: 4.0 }));
    assert_eq!(Change::powerup(pw::SUPERJUMP as c_int, -1.0), Err(INVALID), "no normal length");
    assert_eq!(Change::powerup(slowmo, 0.0), Err(INVALID));
    assert_eq!(Change::powerup(slowmo, f32::INFINITY), Err(INVALID));
    assert_eq!(Change::powerup(slowmo | pw::SLOWPAUSE as c_int, 5.0), Err(INVALID), "one at a time");
    for instant in [pw::KUNGFU, pw::GRENADE, pw::HEALTH, pw::ARMOR] {
        assert_eq!(Change::powerup(instant as c_int, 5.0), Err(INVALID), "{instant:#x} doesn't run");
    }
    assert_eq!(Change::powerup(0x10000 | slowmo, 5.0), Err(INVALID));
    assert_eq!(Change::powerup(-1, 5.0), Err(INVALID));
}

#[test]
fn snapshot_reports_the_port_state() {
    ts30_gameplay::tables::make_default();
    let mut p = Player { cash: 1234, joined: true, active_powerup: pw::SLOWMO, powerup_until: 50.0, kungfu: true, ..Player::default() };
    p.ts.award = 4;
    p.ts.spree = 3;
    p.ts.held = pw::SLOWMO;
    p.ts.held_secs = 5;
    p.inv.sv.free_slots = 41;
    p.stunt.stance = 2;
    let v = entvars_t { fuser1: 0.35, ..Default::default() };
    let s = snapshot(&p, &v);
    assert_eq!((s.joined, s.cash, s.free_slots, s.award, s.kill_streak), (1, 1234, 41, 4, 3));
    assert_eq!((s.active_powerup, s.powerup_until, s.stored_powerup, s.stored_powerup_value), (1, 50.0, 1, 5));
    assert_eq!((s.kungfu, s.superjump, s.time_scale, s.stunt_state), (1, 0, 0.35, 2));
    // Kung fu: `WeaponInfo`'s bar in the clip.
    assert_eq!((s.weapon, s.clip, s.ammo, s.fire_mode, s.attachments), (0, 100, 0, 0, 0));

    use shared::weapons::id;
    p.inv.gun.take_weapon(id::AK47, None, 0).unwrap();
    p.inv.gun.current = id::AK47;
    p.inv.gun.status[id::AK47 as usize].clip = 17;
    let t = shared::weapons::info(id::AK47).unwrap().ammo as usize;
    p.inv.gun.reserve[t] = 60;
    p.active_powerup = 0;
    let s = snapshot(&p, &v);
    assert_eq!((s.weapon, s.clip, s.ammo), (id::AK47 as c_int, 17, 60));
    assert_eq!(s.powerup_until, 0.0, "nothing running");
}

#[test]
fn weapon_ammo_sets_the_owned_record_and_flags_the_hud() {
    ts30_gameplay::tables::make_default();
    use crate::weapons::{Inventory, set_weapon_ammo};
    use shared::weapons::id;
    let mut inv = Inventory::default();
    assert!(!set_weapon_ammo(&mut inv, id::M4, Some(5), None), "not owned");
    inv.gun.take_weapon(id::M4, None, 0).unwrap();
    inv.gun.current = id::M4;
    inv.sv.sent_id = Some(id::M4);
    inv.sv.sent_owned[id::M4 as usize] = true;
    inv.sv.next_update = 99.0;
    assert!(set_weapon_ammo(&mut inv, id::M4, Some(5), Some(77)));
    assert_eq!(inv.gun.status[id::M4 as usize].clip, 5);
    assert_eq!(inv.gun.clip_mirror, 5);
    assert_eq!(inv.gun.current_reserve(), 77);
    assert_eq!(
        (inv.sv.sent_id, inv.sv.sent_owned[id::M4 as usize], inv.sv.next_update),
        (None, false, 0.0),
        "WeaponInfo and WStatus go out"
    );
    // Leaving both alone still resends.
    assert!(set_weapon_ammo(&mut inv, id::M4, None, None));
    assert_eq!(inv.gun.status[id::M4 as usize].clip, 5);

    // Blades have no reserve.
    inv.gun.take_weapon(id::KNIFE, None, 0).unwrap();
    assert!(!set_weapon_ammo(&mut inv, id::KNIFE, Some(3), Some(1)));
    assert!(set_weapon_ammo(&mut inv, id::KNIFE, Some(3), None));
    assert_eq!(inv.gun.status[id::KNIFE as usize].clip, 3);
}

#[test]
fn weapon_ammo_splits_an_akimbo_clip_as_a_reload_does() {
    ts30_gameplay::tables::make_default();
    use crate::weapons::{Inventory, set_weapon_ammo};
    let (single, pair) = (2u8, 8u8); // Beretta, Akimbo Berettas
    assert_eq!(shared::weapons::info(pair).unwrap().single, single as i32);
    let mut inv = Inventory::default();
    inv.gun.status[single as usize].owned = true;
    inv.gun.status[pair as usize].owned = true;
    assert!(set_weapon_ammo(&mut inv, pair, Some(21), None));
    let p = inv.gun.status[pair as usize];
    assert_eq!((p.clip, p.left, p.right), (21, 10, 11));
    assert_eq!(inv.gun.status[single as usize].clip, 10, "the single gun is the left one");
    // The single's clip goes into the pair's left gun.
    assert!(set_weapon_ammo(&mut inv, single, Some(15), None));
    let p = inv.gun.status[pair as usize];
    assert_eq!((p.clip, p.left), (26, 15));
    assert!(!inv.sv.sent_owned[single as usize] && !inv.sv.sent_owned[pair as usize]);
}

#[test]
fn plugin_speed_floors_and_ends() {
    let mut p = Player::default();
    powerup::set_plugin_speed(&mut p, 10.0, 0.0, 200.0, 5.0);
    assert_eq!(p.ts.plugin_speed, Some(powerup::PluginSpeed { amount: 0.01, aura: 200.0, until: 15.0 }), "TS's floor");
    p.ts.next_flow = 12.0;
    powerup::set_plugin_speed(&mut p, 11.0, 0.5, 0.0, 2.0);
    assert_eq!(p.ts.plugin_speed.unwrap().until, 13.0);
    assert_eq!(p.ts.next_flow, 0.0, "applied on the next think");
    powerup::set_plugin_speed(&mut p, 12.0, 1.0, 0.0, 0.0);
    assert_eq!(p.ts.plugin_speed.unwrap().until, 0.0, "duration 0 ends it at the next think");
    let mut q = Player::default();
    powerup::set_plugin_speed(&mut q, 12.0, 1.0, 0.0, 0.0);
    assert_eq!(q.ts.plugin_speed, None, "nothing to end");
}

#[test]
fn physics_speed_replaces_the_tsslowmo_amount() {
    let rules = crate::gameplay::TEST_POWERUPS;
    let mut p = Player { active_powerup: pw::SLOWMO, ..Player::default() };
    assert_eq!(powerup::hud_slow_amount(&p, &rules), 0.35);
    p.ts.physics_speed = Some(0.01);
    assert_eq!(powerup::hud_slow_amount(&p, &rules), 0.01);
}

#[test]
fn private_blocks_cover_ts_objects() {
    // `CBot` (mp.dll) plus the 5-int Linux shift; `CTSGun` and the highest int old plugins write
    // (`tsxaddon.inc`: 850, +5 on Linux); `CTSKnife` and the stock tsx thrower read (int 35).
    const { assert!(crate::world::PLAYER_PRIVATE_SIZE >= 0x25f0 + 5 * 4) };
    const { assert!(crate::world::TSGUN_PRIVATE_SIZE >= 0xa10 + 5 * 4) };
    const { assert!(crate::world::TSGUN_PRIVATE_SIZE >= (850 + 5 + 1) * 4) };
    const { assert!(crate::world::KNIFE_PRIVATE_SIZE >= 0xa4 + 5 * 4) };
}
