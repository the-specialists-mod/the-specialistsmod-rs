//! The server's gameplay plugins (`docs/implementation/gameplay-plugins.md`): the registry the
//! server library fills at `GameDLLInit`, the active plugin, and where each part of the server
//! finds its rules.
//!
//! * Movement (`pfnPM_Move`): [`pm_rules`], from the player's physinfo key `gp`, as the client
//!   predicts it.
//! * The gun ([`weapons::host`](crate::weapons::host)): [`gun_rules`], from the same physinfo key,
//!   once per command.
//! * Everything else (the frame, rules, scoring): [`active`], the plugin `ts_gameplay` selected
//!   when the map started.
//!
//! `ts_gameplay` is latched at a map start ([`map_select`]), whose new game is the fresh state.
//! A change mid-map, from any source, reloads the map ([`crate::reload`]). Physinfo `gp` and
//! `SrvSett` 4 carry the id to the clients.

pub mod server;

pub use server::{ConnectingClient, ServerRules};

use crate::rules::cvars;
use goldsrc::Global;
use goldsrc::pmove::PlayerMoveExt;
use goldsrc::server::engine;
use goldsrc::sys::{edict_t, playermove_t};
use shared::gameplay::{GunRules, PHYSINFO_KEY_C, PmRules};
use std::cell::{Cell, RefCell};
use std::sync::{PoisonError, RwLock};
use ts_gameplay::Registry;

/// A gameplay plugin as the server sees it: its shared hooks and the server's. Each part is its
/// own object, so a plugin's server half can live in a package of its own (the server library
/// links it; the client library never does).
pub struct ServerPlugin {
    pub pm: &'static dyn PmRules,
    pub gun: &'static dyn GunRules,
    pub server: &'static dyn ServerRules,
}

/// A stand-in ruleset for this crate's tests, which run with nothing registered: inert answers
/// for the hooks they reach (the rulesets' own crates test their bodies).
#[cfg(test)]
pub(crate) struct Defaults;

/// Neutral stand-ins for the rules hooks, which this crate's tests don't reach: the 3.0 plugin's
/// own tests check its bodies.
#[cfg(test)]
impl ServerRules for Defaults {
    fn on_activate(&self) {}
    fn refuse_client(&self, _client: ConnectingClient) -> Option<&'static str> {
        None
    }
    fn game_description(&self, _mode: crate::rules::DescriptionMode) -> &'static std::ffi::CStr {
        crate::game_description!()
    }
    fn cvars(&self) -> &'static [server::CvarDef] {
        &[]
    }
    fn server_command(&self, _name: &str) -> bool {
        false
    }
    fn client_command(
        &self,
        _p: &mut crate::player::Player,
        _cx: &mut crate::world::Cx,
        _cmd: &str,
        _args: &crate::commands::Args,
    ) -> Option<bool> {
        None
    }
    fn install(&self, _g: &mut crate::rules::state::GameRules) {
        unreachable!("not in unit tests")
    }
    fn think(&self, _now: f32) {
        unreachable!("not in unit tests")
    }
    fn set_player_the_one(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {
        unreachable!("not in unit tests")
    }
    fn default_team(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) -> Vec<u8> {
        unreachable!("not in unit tests")
    }
    fn on_spawn(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {}
    fn put_in_server_cash(&self) -> i32 {
        0
    }
    fn on_start_observer(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {}
    fn observer_think(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _now: f32) {
        unreachable!("not in unit tests")
    }
    fn observer_set_mode(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _mode: i32) {
        unreachable!("not in unit tests")
    }
    fn on_init_hud(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {}
    fn set_respawn_time(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _secs: f32) {
        unreachable!("not in unit tests")
    }
    fn on_killed(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {}
    fn change_team_tail(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _kill: bool) {
        unreachable!("not in unit tests")
    }
    fn health_regen(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _now: f32) {
        unreachable!("not in unit tests")
    }
    fn points_for_frag(&self, _frag: &crate::rules::scoring::Frag<'_>, _sliding: &mut bool, _diving: &mut bool) -> i32 {
        unreachable!("not in unit tests")
    }
    fn player_killed(
        &self,
        _p: &mut crate::player::Player,
        _cx: &mut crate::world::Cx,
        _d: &crate::damage::DamageInfo,
        _g: crate::damage::glast::Snapshot,
    ) {
        unreachable!("not in unit tests")
    }
    fn cash_based(&self) -> bool {
        false
    }
    fn buy_armament(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {
        unreachable!("not in unit tests")
    }
    fn precache(&self) {}
    fn outgoing_message(
        &self,
        _msg: crate::messages::Msg,
        w: goldsrc::server::MsgWriter,
        send: &mut dyn FnMut(goldsrc::server::MsgWriter),
    ) {
        send(w);
    }
    // The inventory, entity, power-up, stunt, damage, melee and explosion hooks: no test here
    // reaches them but `powerups` (the plugin API's power-up checks); their 3.0 bodies are tested
    // in the 3.0 plugin.
    fn pick_up_cost(&self, _id: u8, _w: &shared::weapons::WeaponInfo, _from: Option<&shared::weapons::Status>) -> Option<f32> {
        unimplemented!("pick_up_cost")
    }
    fn dead_grenades_become_powerups(&self) -> bool {
        unimplemented!("dead_grenades_become_powerups")
    }
    fn drop_lifetime_and_glow(&self) -> bool {
        unimplemented!("drop_lifetime_and_glow")
    }
    fn stack_drop_count(&self, _held: i32, _disarm: bool) -> i32 {
        unimplemented!("stack_drop_count")
    }
    fn world_gun_fall_impact(&self) -> bool {
        unimplemented!("world_gun_fall_impact")
    }
    fn world_gun_touch(
        &self,
        _gun: *mut goldsrc::sys::edict_t,
        _attacker: *mut goldsrc::sys::edict_t,
        _id: u8,
        _other: *mut goldsrc::sys::edict_t,
    ) {
        unimplemented!("world_gun_touch")
    }
    fn ammo_pack_stays_when_nothing_fits(&self) -> bool {
        unimplemented!("ammo_pack_stays_when_nothing_fits")
    }
    fn player_add_health(&self, _v: &mut goldsrc::sys::entvars_t, _amount: f32) -> Option<bool> {
        unimplemented!("player_add_health")
    }
    fn battery_armor(&self, _p: *mut goldsrc::sys::edict_t, _amount: f32) {
        unimplemented!("battery_armor")
    }
    fn charger_capacity(&self, _armor: bool) -> i32 {
        unimplemented!("charger_capacity")
    }
    fn charger_charge(&self, _armor: bool, _juice: &mut i32, _activator: *mut goldsrc::sys::edict_t) {
        unimplemented!("charger_charge")
    }
    fn entity_object(&self, _class: &std::ffi::CStr) -> Option<Box<dyn crate::world::Entity>> {
        unimplemented!("entity_object")
    }
    fn spawns_entity(&self, _class: &std::ffi::CStr) -> bool {
        unimplemented!("spawns_entity")
    }
    fn entity_key_value(
        &self,
        _ent: &mut dyn crate::world::Entity,
        _cx: &mut crate::world::Cx,
        _key: &std::ffi::CStr,
        _value: &std::ffi::CStr,
    ) -> Option<bool> {
        unimplemented!("entity_key_value")
    }
    fn entity_touch(&self, _ent: &mut dyn crate::world::Entity, _cx: &mut crate::world::Cx, _other: *mut goldsrc::sys::edict_t) -> bool {
        unimplemented!("entity_touch")
    }
    fn objectives(&self) -> &'static crate::entities::ts::objective::ObjectiveRules {
        unimplemented!("objectives")
    }
    fn round_time_left(&self, _now: f32) {
        unimplemented!("round_time_left")
    }
    fn giver_team_win(&self, _w: &crate::entities::ts::ctb::GiverWin) {
        unimplemented!("giver_team_win")
    }
    fn break_debris_from_hit(&self) -> bool {
        unimplemented!("break_debris_from_hit")
    }
    fn breakable_attack_dir(
        &self,
        _inflictor_origin: shared::weapons::spread::Vec3,
        _absmin: shared::weapons::spread::Vec3,
        _size: shared::weapons::spread::Vec3,
    ) -> shared::weapons::spread::Vec3 {
        unimplemented!("breakable_attack_dir")
    }
    fn broken_respawn_delay(&self, _respawn_time: f32) -> Option<f32> {
        unimplemented!("broken_respawn_delay")
    }
    fn breakable_respawn_guarded(&self) -> bool {
        unimplemented!("breakable_respawn_guarded")
    }
    fn respawn_entities(&self) {
        unimplemented!("respawn_entities")
    }
    fn entities_think(&self, _now: f32) {
        unimplemented!("entities_think")
    }
    fn map_globals(&self, _e: *mut goldsrc::sys::edict_t, _spawnflags: i32) -> Option<crate::entities::ts::misc::MapFlags> {
        unimplemented!("map_globals")
    }
    fn ground_weapon_lands_flying(&self) -> bool {
        unimplemented!("ground_weapon_lands_flying")
    }
    fn give_ground_weapon(
        &self,
        _g: &mut crate::entities::ts::ground::GroundWeapon,
        _cx: &mut crate::world::Cx,
        _p: &mut crate::player::Player,
        _pcx: &mut crate::world::Cx,
    ) {
        unimplemented!("give_ground_weapon")
    }
    fn powerups(&self) -> &'static crate::rules::powerup::PowerupRules {
        &TEST_POWERUPS
    }
    fn powerup_kind(&self, _key: i32, _spawnflags: i32) -> (u16, bool) {
        unimplemented!("powerup_kind")
    }
    fn armor_powerup(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _armor: f32) {
        unimplemented!("armor_powerup")
    }
    fn time_scale_eases(&self) -> bool {
        unimplemented!("time_scale_eases")
    }
    fn on_go_slow(&self, _p: &mut crate::player::Player, _amount: f32) {
        unimplemented!("on_go_slow")
    }
    fn grenade_fuse_step(&self, _elapsed: f32, _time_scale: f32) -> f32 {
        unimplemented!("grenade_fuse_step")
    }
    fn anim_framerate(&self, _time_scale: f32) -> f32 {
        unimplemented!("anim_framerate")
    }
    fn motion_blur(&self) -> bool {
        unimplemented!("motion_blur")
    }
    fn sound_pitch(&self, _pitch: i32, _time_scale: f32, _s: f32) -> i32 {
        unimplemented!("sound_pitch")
    }
    fn stunts(&self) -> &'static crate::stunt::rules::StuntRules {
        unimplemented!("stunts")
    }
    fn stunt_clock(&self) -> &'static dyn crate::stunt::rules::StuntClock {
        unimplemented!("stunt_clock")
    }
    fn can_dive(&self, _p: &crate::player::Player, _armor: f32, _dive: crate::stunt::rules::Dive) -> bool {
        unimplemented!("can_dive")
    }
    fn bullet_time_allowed(&self, _time_scale: f32) -> bool {
        unimplemented!("bullet_time_allowed")
    }
    fn bullet_trace(
        &self,
        _start: shared::weapons::spread::Vec3,
        _end: shared::weapons::spread::Vec3,
        _shooter: *mut goldsrc::sys::edict_t,
    ) -> goldsrc::sys::TraceResult {
        unimplemented!("bullet_trace")
    }
    fn bullet_range_index(&self, _scaled: shared::pm::Ext) -> i32 {
        unimplemented!("bullet_range_index")
    }
    fn bullet_damage(&self, _ammo: u8, _weapon: u8, _damage: f32, _falloff: f32, _simulated: bool) -> f32 {
        unimplemented!("bullet_damage")
    }
    fn bullet_damage_multiplier(&self, _acc: &mut f32, _cheat: Option<f32>) -> f32 {
        unimplemented!("bullet_damage_multiplier")
    }
    fn pierce_step(&self, _left: f32) -> f32 {
        unimplemented!("pierce_step")
    }
    fn weapon_realbullet(&self, _realbullet: i32) -> i32 {
        unimplemented!("weapon_realbullet")
    }
    fn bullet_velocity_mult(&self, _arith: shared::pm::Arith, _slowmul: f32, _bullet_time: bool) -> shared::pm::Ext {
        unimplemented!("bullet_velocity_mult")
    }
    fn player_hit(&self, _hit: &crate::player::damage::PlayerHit) -> crate::player::damage::HitDamage {
        unimplemented!("player_hit")
    }
    fn armor_damage(&self, _armor: &mut f32, _amount: f32, _hitgroup: i32, _bits: i32) -> f32 {
        unimplemented!("armor_damage")
    }
    fn damage_punch(&self) -> f32 {
        unimplemented!("damage_punch")
    }
    fn melee_block(
        &self,
        _kind: shared::weapons::gun::MeleeKind,
        _attacker: *mut goldsrc::sys::edict_t,
        _victim: *mut goldsrc::sys::edict_t,
        _damage: f32,
        _atk_block: f32,
    ) -> Option<shared::weapons::gun::MeleeResult> {
        unimplemented!("melee_block")
    }
    fn jump_kick_hit(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx, _strength: f32) {
        unimplemented!("jump_kick_hit")
    }
    fn jump_kick_attacker_block(&self, _gun: &mut shared::weapons::Gun) -> f32 {
        unimplemented!("jump_kick_attacker_block")
    }
    fn backflip_kick(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {
        unimplemented!("backflip_kick")
    }
    fn zero_delayed_hit_reaches_player(&self) -> bool {
        unimplemented!("zero_delayed_hit_reaches_player")
    }
    fn melee_disarm(&self, _damage: f32, _victim_is_the_one: bool) -> Option<f32> {
        unimplemented!("melee_disarm")
    }
    fn grenade_explosion(&self) -> (f32, f32, f32) {
        unimplemented!("grenade_explosion")
    }
    fn explosion_pushes_world_guns(&self) -> bool {
        unimplemented!("explosion_pushes_world_guns")
    }
    fn explosion_aftermath(
        &self,
        _org: shared::weapons::spread::Vec3,
        _inflictor: *mut goldsrc::sys::edict_t,
        _attacker: *mut goldsrc::sys::edict_t,
    ) {
        unimplemented!("explosion_aftermath")
    }
    fn thrown_blade(&self, _btype: i32) -> crate::weapons::ThrownBlade {
        unimplemented!("thrown_blade")
    }
    fn thrown_blade_damage(&self, _btype: i32, _from: shared::weapons::spread::Vec3, _at: shared::weapons::spread::Vec3) -> f32 {
        unimplemented!("thrown_blade_damage")
    }

    // Audit round 2: B1
    fn say_rules(&self) -> &'static crate::rules::commands::SayRules {
        unreachable!("not in unit tests")
    }
    fn say_dead_chat_gated(&self) -> bool {
        unreachable!("not in unit tests")
    }
    fn client_kill(&self, _p: &mut crate::player::Player, _cx: &mut crate::world::Cx) {
        unreachable!("not in unit tests")
    }
    fn killed_resets_view(&self) -> bool {
        unreachable!("not in unit tests")
    }

    // Audit round 2: B2
    fn init_hud(&self) -> &'static server::InitHudRules {
        unimplemented!("init_hud")
    }
    fn team_changes(&self) -> &'static server::TeamChangeRules {
        unimplemented!("team_changes")
    }
    fn spawn_spot_master_test(&self) -> bool {
        unimplemented!("spawn_spot_master_test")
    }

    // Audit round 2: D2

    fn ground_weapon_pairs_to_single(&self) -> bool {
        unimplemented!("ground_weapon_pairs_to_single")
    }
    fn random_position_ray(&self, _angle: i32, _dist: i32, _from: [f32; 3]) -> ([f32; 3], [f32; 3]) {
        unimplemented!("random_position_ray")
    }

    // Audit round 2: C

    fn screen_effects(&self) -> crate::entities::stock::explode::ScreenEffects {
        unimplemented!("screen_effects")
    }
    fn delayed_hit_dir(&self, _stunt_dir: shared::weapons::spread::Vec3) -> shared::weapons::spread::Vec3 {
        unimplemented!("delayed_hit_dir")
    }
    fn explosion_hit(
        &self,
        _org: shared::weapons::spread::Vec3,
        _spot: shared::weapons::spread::Vec3,
        _dmg: f32,
        _r_full: f32,
        _r_max: f32,
        _occluder: Option<f32>,
    ) -> Option<(f32, shared::weapons::spread::Vec3)> {
        unimplemented!("explosion_hit")
    }
    fn attack_dir(&self, _from: shared::weapons::spread::Vec3, _to: shared::weapons::spread::Vec3) -> shared::weapons::spread::Vec3 {
        unimplemented!("attack_dir")
    }

    // Audit round 2: D1
    fn ammo_pack_drop(&self, _arith: shared::pm::Arith) -> crate::weapons::worldgun::AmmoPackDrop {
        unimplemented!("ammo_pack_drop")
    }
    fn gun_hud_update(&self, _sv: &mut crate::weapons::SvState, _gun: &shared::weapons::Gun) -> crate::weapons::net::GunHud {
        unimplemented!("gun_hud_update")
    }
    fn map_materials(&self) -> bool {
        unimplemented!("map_materials")
    }
    fn texture_sounds_in_multiplayer(&self) -> bool {
        unimplemented!("texture_sounds_in_multiplayer")
    }
}

/// Power-up values for this crate's tests: the 3.0 plugin's (its own tests pin them).
#[cfg(test)]
pub(crate) const TEST_POWERUPS: crate::rules::powerup::PowerupRules = crate::rules::powerup::PowerupRules {
    base_secs: [3, 3, 0, 2, 3, 0, 0, 0, 0],
    multiplier: true,
    instant: 0x1e6,
    infinite_clip_instant: true,
    slowmo_top_up_below: 3,
    slowmo_swap_above: 2,
    swap_on_instant: false,
    slowmo_drop_above: 2,
    slowmo: (0.6, 0.35, false),
    slowpause: (0.8, 0.225, true),
    hud_slowmo: 0.35,
    hud_slowpause: 0.225,
    hud_normal_as_zero: false,
    grenade_selects: true,
};

/// [`Defaults`] as a plugin, with the 3.0 plugin's movement and gun rules.
#[cfg(test)]
pub(crate) static DEFAULTS: ServerPlugin = ServerPlugin { pm: &ts30_gameplay::TS30, gun: &ts30_gameplay::TS30, server: &Defaults };

/// The registered plugins. The first one the server library registers is the default: the
/// fallback for an empty or unknown id, and the plugin the server runs before its first map.
static REGISTRY: RwLock<Registry<ServerPlugin>> = RwLock::new(Registry::new());

/// A plugin and its id.
type Active = (&'static str, &'static ServerPlugin);

/// The active plugin: `None` before the first map installs its rules (the default stands in).
static ACTIVE: Global<Cell<Option<Active>>> = Global(Cell::new(None));

/// The last unknown id warned about, so each is reported once.
static WARNED: Global<RefCell<String>> = Global(RefCell::new(String::new()));

/// Registers a plugin (the server library's `GameDLLInit`, before the rules' `game_init`), the
/// default ruleset first. False for an id that isn't a short number. The plugins' own cvars
/// ([`ServerRules::cvars`]) are registered later, with the server's ([`register_cvars`]).
pub fn register(id: &'static str, plugin: &'static ServerPlugin) -> bool {
    let added = REGISTRY.write().unwrap_or_else(PoisonError::into_inner).add(id, plugin);
    if added {
        // The first plugin's are the default ruleset's tables (`set_default_tables` keeps the first).
        shared::weapons::table::set_default_tables(plugin.gun.tables());
    }
    added
}

/// Every registered plugin's own cvars ([`ServerRules::cvars`]), in registration order, after the
/// server's (`rules::cvars::register`), each unless the name exists already.
pub fn register_cvars() {
    let r = REGISTRY.read().unwrap_or_else(PoisonError::into_inner);
    for id in r.ids() {
        if let Some(p) = r.resolve(id) {
            cvars::register_extra(p.plugin.server.cvars());
        }
    }
}

/// The default plugin and its id (the first registered).
fn fallback() -> Active {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner).fallback().unwrap_or_else(unregistered)
}

/// The default ruleset's id: `ts_gameplay`'s default value.
pub fn default_id() -> &'static str {
    fallback().0
}

/// What stands in while nothing is registered: nothing in the library, which registers its rulesets
/// at `GameDLLInit` before any game code runs; [`Defaults`] in this crate's tests.
fn unregistered() -> Active {
    #[cfg(test)]
    return ("30", &DEFAULTS);
    #[cfg(not(test))]
    panic!("no gameplay ruleset registered (server-dll registers them at GameDLLInit)");
}

/// The registered ids, for the cvar's help and warnings.
pub fn ids() -> Vec<&'static str> {
    REGISTRY.read().unwrap_or_else(PoisonError::into_inner).ids().collect()
}

/// The plugin for `id`; an unknown one is the default plugin, with a console warning the first
/// time.
fn resolve(id: &str) -> Active {
    let Some(r) = REGISTRY.read().unwrap_or_else(PoisonError::into_inner).resolve(id) else { return unregistered() };
    if r.unknown {
        let id = id.trim();
        let mut w = WARNED.0.borrow_mut();
        if *w != id {
            *w = id.to_owned();
            engine().print(&format!("[ts] gameplay: unknown ruleset \"{id}\" (registered: {}); using \"{}\"\n", ids().join(", "), r.id));
        }
    }
    (r.id, r.plugin)
}

/// The ruleset id `ts_gameplay` selects now (an unknown one is the default plugin's, with the
/// warning): what [`crate::reload`] compares with [`active_id`].
pub fn wanted_id() -> &'static str {
    resolve(&cvars::gameplay()).0
}

/// The active plugin: what the frame, the rules and scoring use.
pub fn active() -> &'static ServerPlugin {
    ACTIVE.0.get().unwrap_or_else(fallback).1
}

/// The active plugin's id: what physinfo `gp` and `SrvSett` 4 carry.
pub fn active_id() -> &'static str {
    ACTIVE.0.get().unwrap_or_else(fallback).0
}

/// The movement rules for one `pfnPM_Move`, from the moving player's physinfo (the key the
/// client predicts with).
pub fn pm_rules(pm: &playermove_t) -> &'static dyn PmRules {
    resolve(&pm.physinfo_value(PHYSINFO_KEY_C)).1.pm
}

/// The gun rules for one player's command, from its physinfo
/// ([`GunHost::rules`](shared::weapons::GunHost::rules)).
pub fn gun_rules(e: *mut edict_t) -> &'static dyn GunRules {
    resolve(&engine().physics_key(e, PHYSINFO_KEY_C)).1.gun
}

/// Sets a client's physinfo `gp` to the active plugin's id: at every spawn, next to `mj`/`hl`.
pub fn set_physinfo(e: *mut edict_t) {
    let id = goldsrc::util::to_cstring(active_id());
    engine().set_physics_key(e, PHYSINFO_KEY_C, &id);
}

/// Calls `f` with the connecting client as `ClientConnect` can tell it
/// ([`ConnectingClient::classify`]), from its edict and userinfo.
pub fn with_connecting_client<R>(e: *mut edict_t, f: impl FnOnce(ConnectingClient) -> R) -> R {
    use goldsrc::edict::EdictPtr;
    let eng = engine();
    let fake = !e.is_null() && e.v().flags & goldsrc::sys::FL_FAKECLIENT as i32 != 0;
    let key = |k: &std::ffi::CStr| if e.is_null() { String::new() } else { eng.info_key(e, k).to_string_lossy().into_owned() };
    let (bot, version) = (key(c"*bot"), key(shared::protocol::port::USERINFO_KEY_C));
    let hltv = [key(c"*hltv"), key(c"hltv")].into_iter().find(|v| !v.is_empty() && v != "0").unwrap_or_default();
    f(ConnectingClient::classify(fake, &bot, &hltv, &version))
}

/// `ClientConnect`: the active ruleset's verdict on the connecting client
/// ([`ServerRules::refuse_client`]). `Some(reason)` refuses it.
pub fn refuse_client(e: *mut edict_t) -> Option<&'static str> {
    with_connecting_client(e, |client| {
        let reason = active().server.refuse_client(client)?;
        engine().print(&format!("[ts] gameplay: refused a client in ruleset {} ({client:?}): {reason}\n", active_id()));
        Some(reason)
    })
}

/// The `SrvSett` 4 value for the active plugin.
pub fn srvsett_value() -> i16 {
    // Registered ids are numbers (`Registry::add`).
    ts_gameplay::srvsett_value(active_id()).unwrap_or_default()
}

/// A map is installing its rules (`rules::install`, from worldspawn, after `game.cfg`): the plugin
/// `ts_gameplay` selects becomes active for the whole map, before the rules are built
/// ([`ServerRules::install`]).
pub fn map_select() {
    let (id, plugin) = resolve(&cvars::gameplay());
    set_active(id, plugin);
    engine().print(&format!("[ts] gameplay: ruleset {id} for this map\n"));
}

/// A map has installed its rules ([`map_select`] chose the plugin): the plugin sets up its state.
pub fn map_start() {
    active().server.on_activate();
}

/// Makes a plugin the active one, with its weapon tables
/// ([`GunRules::tables`](shared::gameplay::GunRules::tables)).
fn set_active(id: &'static str, plugin: &'static ServerPlugin) {
    ACTIVE.0.set(Some((id, plugin)));
    shared::weapons::table::set_ruleset_tables(plugin.gun.tables());
}

/// Every registered plugin's own precaches ([`ServerRules::precache`]), each once, at the map's
/// world precache after the server's own.
pub fn precache_rulesets() {
    let r = REGISTRY.read().unwrap_or_else(PoisonError::into_inner);
    let mut done: Vec<*const ServerPlugin> = Vec::new();
    for id in r.ids() {
        let Some(p) = r.resolve(id).map(|r| r.plugin) else { continue };
        if !done.contains(&(p as *const ServerPlugin)) {
            done.push(p);
            p.server.precache();
        }
    }
}

/// The weapon tables of every registered plugin, in registration order (the default's first)
/// and each once: what the map precaches (`weapons::precache_weapon_assets`), so the map's
/// precache list is the same whichever ruleset runs it.
pub fn ruleset_tables() -> Vec<&'static shared::weapons::Tables> {
    let r = REGISTRY.read().unwrap_or_else(PoisonError::into_inner);
    let mut out: Vec<&'static shared::weapons::Tables> = Vec::new();
    for id in r.ids() {
        let Some(t) = r.resolve(id).map(|r| r.plugin.gun.tables()) else { continue };
        if !out.iter().any(|o| std::ptr::eq(*o, t)) {
            out.push(t);
        }
    }
    out
}
