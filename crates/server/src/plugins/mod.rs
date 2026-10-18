//! `tsmod_*` plugins (port addition, issue #104; docs/implementation/plugin-api.md §1,
//! bot-plugins.md): libraries the server loads from its `dlls/` directory at `GameDLLInit` and
//! calls back on the game's events. `ts-server-dll` does the loading and the C calls
//! ([`PluginHooks`] over a plugin's `ts_plugin_api::Plugin`); this module decides which files to
//! load ([`loader`]), keeps the loaded plugins, and fires their callbacks at the right points.
//!
//! # When callbacks run
//! Plugins call the plugin API from their callbacks, and the API reaches a player only while it
//! is in its slot (`plugin_api`'s module docs). So the player callbacks that the game reaches
//! inside a player's own dispatch (spawn, damage, death, a kill) are deferred until every player
//! they name is back ([`after`]); the rest (frame, map, connect, commands, sounds) run at once.

pub mod host;
pub mod loader;

use crate::commands::Args;
use crate::world::{self, DamageInfo};
use goldsrc::Global;
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::Rc;
use ts_plugin_api::PluginId;

/// A loaded plugin's callbacks. `ts-server-dll` implements it over the plugin's C descriptor;
/// every method does nothing unless the plugin gave that callback. Players and entities are
/// indices (0 the world, -1 none), as the API's are.
pub trait PluginHooks {
    fn game_init(&self) {}
    fn precache(&self) {}
    fn server_activate(&self, _map: &CStr) {}
    fn server_deactivate(&self) {}
    fn start_frame(&self) {}
    fn shutdown(&self) {}
    fn client_connect(&self, _player: i32, _bot: bool) {}
    fn client_put_in_server(&self, _player: i32, _bot: bool) {}
    fn client_disconnect(&self, _player: i32, _bot: bool) {}
    /// True if the plugin swallowed the command.
    fn client_command(&self, _player: i32, _argv: &[&CStr]) -> bool {
        false
    }
    /// Whether [`Self::client_command`] does anything (so the server skips building `argv`).
    fn wants_client_commands(&self) -> bool {
        false
    }
    fn player_spawned(&self, _player: i32) {}
    /// Version 6 (replaces `player_damaged`): a player's damage.
    fn damage(&self, _event: &ts_plugin_api::DamageEvent) {}
    fn player_killed(&self, _victim: i32, _attacker: i32, _inflictor: i32) {}
    fn on_kill(&self, _killer: i32, _victim: i32, _points: i32) {}
    fn round_start(&self) {}
    fn round_end(&self) {}
    /// Version 6 adds the channel and the responsible player.
    #[allow(clippy::too_many_arguments)] // the C callback's arguments
    fn sound(&self, _entity: i32, _origin: [f32; 3], _sample: &CStr, _volume: f32, _attenuation: f32, _channel: i32, _source: i32) {}
    /// Whether [`Self::sound`] does anything (the sound hook is installed only then).
    fn wants_sounds(&self) -> bool {
        false
    }
    fn weapon_changed(&self, _player: i32, _weapon: i32) {}
    fn ammo_changed(&self, _player: i32, _weapon: i32, _clip: i32, _ammo: i32) {}
    /// Version 5: a stunt started (`ts_plugin_api::stunt`).
    fn player_stunt(&self, _player: i32, _stunt: i32) {}
    /// Version 5: a power-up took effect (`ts_plugin_api::powerup` bits).
    fn player_powerup(&self, _player: i32, _powerup: i32) {}
    /// Version 5: a kung-fu move is about to land; may change its time and damage. True blocks it.
    fn melee_attack(&self, _player: i32, _time: &mut f32, _damage: &mut f32) -> bool {
        false
    }
    /// Whether [`Self::melee_attack`] does anything (so the gun skips lending the player).
    fn wants_melee_attack(&self) -> bool {
        false
    }
    /// Version 5: `PlayerKilled` scored a death.
    fn kill_scored(&self, _kill: &ts_plugin_api::KillInfo) {}
    /// Version 6: a bot touched `other` (or was touched by it).
    fn entity_touch(&self, _player: i32, _other: i32) {}
    /// Whether [`Self::entity_touch`] does anything (so the touch path skips the bot test).
    fn wants_entity_touch(&self) -> bool {
        false
    }
    /// Version 6: a player fired a shot.
    fn weapon_fired(&self, _player: i32, _weapon: i32, _origin: [f32; 3], _dir: [f32; 3], _silenced: bool) {}
    /// Version 6: a player picked up an item (`kind` an `ts_plugin_api::entity` kind).
    fn item_pickup(&self, _player: i32, _entity: i32, _kind: i32, _id: i32) {}
    /// Version 8: a client's `tspkg` answer (`state` a `ts_plugin_api::plugin_state` value).
    fn client_plugin_state(&self, _player: i32, _id: &CStr, _version: &CStr, _state: i32, _reason: &CStr) {}
    /// Version 8: a client's `tspkg list` is complete. The entries point into strings that live
    /// for the call.
    fn client_plugin_list(&self, _player: i32, _plugins: &[ts_plugin_api::ClientPlugin]) {}
}

struct Loaded {
    id: PluginId,
    name: String,
    hooks: Option<Rc<dyn PluginHooks>>,
    /// Version 7: a plugin the server didn't load that took an identity (`attach_plugin`).
    attached: bool,
}

/// The plugins being loaded, loaded or attached, and the next id to hand out.
struct Registry {
    next: PluginId,
    list: Vec<Loaded>,
}

impl Registry {
    /// A new entry for `name`: its id, or `None` if a plugin has the name (letter case ignored:
    /// the data directory's name, and Windows file names, don't tell case apart).
    fn add(&mut self, name: &str, attached: bool) -> Option<PluginId> {
        if self.list.iter().any(|l| l.name.eq_ignore_ascii_case(name)) {
            return None;
        }
        // Ids are never reused (#291): a failed or detached plugin may still hold its id.
        let id = self.next;
        self.next = id.checked_add(1)?;
        self.list.push(Loaded { id, name: name.to_owned(), hooks: None, attached });
        Some(id)
    }

    /// Drops the entry `id` (if `attached` says it is that kind): true if it was there.
    fn remove(&mut self, id: PluginId, attached: bool) -> bool {
        let before = self.list.len();
        self.list.retain(|l| l.id != id || l.attached != attached);
        self.list.len() != before
    }
}

static PLUGINS: Global<RefCell<Registry>> = Global(RefCell::new(Registry { next: 1, list: Vec::new() }));

/// Reserves an id for the plugin `name` about to be loaded (its load function may already call
/// the API with it). [`attach`] completes it, [`forget`] drops it if the load fails. `None` if an
/// attached plugin (or another library) has the name.
pub fn reserve(name: &str) -> Option<PluginId> {
    PLUGINS.0.borrow_mut().add(name, false)
}

/// The plugin loaded: its callbacks from now on.
pub fn attach(id: PluginId, hooks: Rc<dyn PluginHooks>) {
    if let Some(l) = PLUGINS.0.borrow_mut().list.iter_mut().find(|l| l.id == id) {
        l.hooks = Some(hooks);
    }
}

/// The load failed: whatever the plugin registered goes (its commands then do nothing).
pub fn forget(id: PluginId) {
    PLUGINS.0.borrow_mut().remove(id, false);
    host::forget(id);
    crate::bot_backend::forget_plugin(id);
}

/// `PluginApi::attach_plugin` (version 7): a new id for a plugin the server didn't load, `EXISTS`
/// if a plugin has the name, `INVALID` for a bad one.
pub fn attach_plugin(name: &str) -> PluginId {
    if !loader::valid_name(name) {
        return ts_plugin_api::INVALID;
    }
    PLUGINS.0.borrow_mut().add(name, true).unwrap_or(ts_plugin_api::EXISTS)
}

/// `PluginApi::detach_plugin` (version 7): an attached plugin gives its identity up. Its
/// commands, listeners and bot backend go (an active backend hands over to native, its bots
/// kicked: `bot_backend::drop_plugin`). False for an id that isn't attached.
pub fn detach_plugin(id: PluginId) -> bool {
    if id == ts_plugin_api::NO_PLUGIN || !PLUGINS.0.borrow_mut().remove(id, true) {
        return false;
    }
    host::forget(id);
    LISTENERS.0.borrow_mut().remove_owned(id);
    crate::bot_backend::drop_plugin(id);
    true
}

/// Whether `id` is an attached plugin's (version 7).
pub fn is_attached(id: PluginId) -> bool {
    id != ts_plugin_api::NO_PLUGIN && PLUGINS.0.borrow().list.iter().any(|l| l.id == id && l.attached)
}

/// The name (`<name>` of `tsmod_<name>`, or the attached name) of a plugin being loaded, loaded or
/// attached.
pub fn name_of(id: PluginId) -> Option<String> {
    PLUGINS.0.borrow().list.iter().find(|l| l.id == id).map(|l| l.name.clone())
}

/// The loaded plugins' names, in load order.
pub fn names() -> Vec<String> {
    PLUGINS.0.borrow().list.iter().filter(|l| l.hooks.is_some()).map(|l| l.name.clone()).collect()
}

/// Listeners (version 5, `PluginApi::add_listener`): plugins the server didn't load (metamod
/// plugins) that registered a descriptor, by handle.
static LISTENERS: Global<RefCell<Listeners>> = Global(RefCell::new(Listeners { next: 1, list: Vec::new() }));

/// A listener: its handle, its owner (version 7: `NO_PLUGIN` or an attached plugin) and callbacks.
struct Listener {
    handle: i32,
    owner: PluginId,
    hooks: Rc<dyn PluginHooks>,
}

/// The listeners in registration order.
struct Listeners {
    next: i32,
    list: Vec<Listener>,
}

impl Listeners {
    fn add(&mut self, owner: PluginId, hooks: Rc<dyn PluginHooks>) -> i32 {
        let h = self.next;
        self.next = h.checked_add(1).unwrap_or(1);
        self.list.push(Listener { handle: h, owner, hooks });
        h
    }

    fn remove(&mut self, handle: i32) -> bool {
        let before = self.list.len();
        self.list.retain(|l| l.handle != handle);
        self.list.len() != before
    }

    /// Drops the listeners of a detached plugin.
    fn remove_owned(&mut self, owner: PluginId) {
        self.list.retain(|l| l.owner != owner);
    }
}

/// Registers a listener for `owner` (`NO_PLUGIN` or an attached plugin): its handle (> 0), or
/// `None` for another owner. Installs the sound hook if it listens for sounds (a loaded plugin's
/// is installed at `GameDLLInit`).
pub fn add_listener(owner: PluginId, hooks: Rc<dyn PluginHooks>) -> Option<i32> {
    if owner != ts_plugin_api::NO_PLUGIN && !is_attached(owner) {
        return None;
    }
    if hooks.wants_sounds() {
        goldsrc::server::set_sound_hook(Some(sound));
    }
    Some(LISTENERS.0.borrow_mut().add(owner, hooks))
}

/// Unregisters a listener: false for a handle that isn't registered.
pub fn remove_listener(handle: i32) -> bool {
    LISTENERS.0.borrow_mut().remove(handle)
}

/// The loaded plugins' callbacks, then the listeners', copied out so a callback may load,
/// register or call back in.
fn all() -> Vec<Rc<dyn PluginHooks>> {
    let mut v: Vec<_> = PLUGINS.0.borrow().list.iter().filter_map(|l| l.hooks.clone()).collect();
    v.extend(LISTENERS.0.borrow().list.iter().map(|l| l.hooks.clone()));
    v
}

/// Whether any plugin is loaded or attached, or a listener registered.
pub fn any() -> bool {
    PLUGINS.0.borrow().list.iter().any(|l| l.hooks.is_some() || l.attached) || !LISTENERS.0.borrow().list.is_empty()
}

fn each(f: impl Fn(&dyn PluginHooks)) {
    for h in all() {
        f(&*h);
    }
}

/// Runs `job` once every player in `players` is back in its slot (`world::defer` on each in
/// turn): at once outside their dispatches.
fn after(players: &[*mut edict_t], job: impl FnOnce() + 'static) {
    match players.split_first() {
        None => job(),
        Some((&first, rest)) => {
            let rest = rest.to_vec();
            if first.is_null() || !world::is_taken_out(first) {
                after(&rest, job);
            } else {
                world::defer(first, move || after(&rest, job));
            }
        }
    }
}

/// An entity's index for a callback: -1 for none.
fn index(e: *mut edict_t) -> i32 {
    if e.is_null() { -1 } else { engine().index_of(e) }
}

fn is_fake(e: *mut edict_t) -> bool {
    !e.is_null() && e.v().flags & FL_FAKECLIENT as i32 != 0
}

// ---- events -------------------------------------------------------------------------------------

/// `GameDLLInit`, after every plugin loaded. Installs the sound hook if a plugin listens.
pub fn game_init() {
    if all().iter().any(|h| h.wants_sounds()) {
        goldsrc::server::set_sound_hook(Some(sound));
    }
    each(|h| h.game_init());
}

pub fn precache() {
    each(|h| h.precache());
}

pub fn server_activate() {
    // Version 6: for every caller of `game_state`, metamod plugins without a listener included.
    crate::plugin_api::world_info::map_started();
    if !any() {
        return;
    }
    let map = engine().string(engine().globals().mapname).to_owned();
    each(|h| h.server_activate(&map));
}

pub fn server_deactivate() {
    each(|h| h.server_deactivate());
}

/// `StartFrame`, after the bots and before the rules: the frame's snapshot for the version 6
/// queries (`plugin_api::frame::begin`), then the plugins' frame. The weapon and ammo changes
/// since the last frame follow after the rules (`plugin_api::start_frame`).
pub fn start_frame() {
    if any() {
        crate::plugin_api::frame::begin();
    } else {
        crate::plugin_api::frame::invalidate();
    }
    each(|h| h.start_frame());
}

pub fn shutdown() {
    each(|h| h.shutdown());
}

pub fn client_connect(e: *mut edict_t) {
    let (i, bot) = (index(e), is_fake(e));
    each(|h| h.client_connect(i, bot));
}

pub fn client_put_in_server(e: *mut edict_t, bot: bool) {
    let i = index(e);
    each(|h| h.client_put_in_server(i, bot));
}

pub fn client_disconnect(e: *mut edict_t) {
    let (i, bot) = (index(e), is_fake(e));
    each(|h| h.client_disconnect(i, bot));
}

/// A client command, before the game's: true if a plugin swallowed it.
pub fn client_command(e: *mut edict_t, args: &Args) -> bool {
    let hooks: Vec<_> = all().into_iter().filter(|h| h.wants_client_commands()).collect();
    if hooks.is_empty() {
        return false;
    }
    let argv: Vec<&CStr> = (0..args.argc()).map(|i| args.argv(i)).collect();
    let i = index(e);
    hooks.iter().any(|h| h.client_command(i, &argv))
}

pub fn player_spawned(e: *mut edict_t) {
    if any() {
        let i = index(e);
        after(&[e], move || each(|h| h.player_spawned(i)));
    }
}

/// Version 6: player `e`'s `TakeDamage` ran (the event filled by `plugin_api::events`), deferred
/// until the victim and attacker are back in their slots.
pub fn damage(e: *mut edict_t, attacker: *mut edict_t, event: ts_plugin_api::DamageEvent) {
    if any() {
        after(&[e, attacker], move || each(|h| h.damage(&event)));
    }
}

pub fn player_killed(e: *mut edict_t, dmg: &DamageInfo) {
    if any() {
        let (v, a, inf) = (index(e), index(dmg.attacker), index(dmg.inflictor));
        after(&[e, dmg.attacker], move || each(|h| h.player_killed(v, a, inf)));
    }
}

pub fn on_kill(killer: *mut edict_t, victim: *mut edict_t, points: i32) {
    if any() {
        let (k, v) = (index(killer), index(victim));
        after(&[killer, victim], move || each(|h| h.on_kill(k, v, points)));
    }
}

pub fn round_start() {
    each(|h| h.round_start());
}

pub fn round_end() {
    each(|h| h.round_end());
}

/// The sound hook (`goldsrc::server::set_sound_hook`): every sound the game plays. `pos` is the
/// ambient sound's position, else the entity's origin. Version 6: the channel, and the player
/// responsible (`plugin_api::source_player`).
fn sound(e: *mut edict_t, pos: Option<[f32; 3]>, sample: &CStr, volume: f32, attenuation: f32, channel: i32) {
    let hooks: Vec<_> = all().into_iter().filter(|h| h.wants_sounds()).collect();
    if hooks.is_empty() {
        return;
    }
    let i = if e.is_null() { 0 } else { engine().index_of(e) };
    let origin = pos.unwrap_or_else(|| if e.is_null() { [0.0; 3] } else { e.v().origin });
    let source = crate::plugin_api::source_player(i);
    for h in hooks {
        h.sound(i, origin, sample, volume, attenuation, channel, source);
    }
}

pub fn weapon_changed(player: i32, weapon: i32) {
    each(|h| h.weapon_changed(player, weapon));
}

pub fn ammo_changed(player: i32, weapon: i32, clip: i32, ammo: i32) {
    each(|h| h.ammo_changed(player, weapon, clip, ammo));
}

/// Version 5: player `e` started a stunt (`ts_plugin_api::stunt`), from inside its PreThink, so
/// the callback waits until it is back in its slot.
pub fn player_stunt(e: *mut edict_t, stunt: i32) {
    if any() {
        let i = index(e);
        after(&[e], move || each(|h| h.player_stunt(i, stunt)));
    }
}

/// Version 5: a power-up took effect on player `e` (deferred as [`player_stunt`]).
pub fn player_powerup(e: *mut edict_t, bits: i32) {
    if any() {
        let i = index(e);
        after(&[e], move || each(|h| h.player_powerup(i, bits)));
    }
}

/// Version 5: `PlayerKilled` scored `victim`'s death by `killer` (null for none), deferred as
/// [`on_kill`] (so after the victim's `Killed`, its `TSHealth` 0 included).
pub fn kill_scored(victim: *mut edict_t, killer: *mut edict_t, kill: ts_plugin_api::KillInfo) {
    if any() {
        after(&[killer, victim], move || each(|h| h.kill_scored(&kill)));
    }
}

/// Whether anything listens for [`entity_touch`]. Asked on every touch, so it looks without
/// copying the lists out.
pub fn wants_entity_touch() -> bool {
    PLUGINS.0.borrow().list.iter().any(|l| l.hooks.as_ref().is_some_and(|h| h.wants_entity_touch()))
        || LISTENERS.0.borrow().list.iter().any(|l| l.hooks.wants_entity_touch())
}

/// Version 6: `DispatchTouch(e, other)` ran. When one of them is a bot (`FL_FAKECLIENT`), the
/// listeners hear (bot, the other) once the bot is back in its slot.
pub fn entity_touch(e: *mut edict_t, other: *mut edict_t) {
    if e.is_null() || other.is_null() {
        return;
    }
    let (bot, thing) = if is_fake(e) && crate::rules::is_player(e) {
        (e, other)
    } else if is_fake(other) && crate::rules::is_player(other) {
        (other, e)
    } else {
        return;
    };
    if !wants_entity_touch() {
        return;
    }
    let (b, t) = (index(bot), index(thing));
    after(&[bot], move || {
        for h in all().iter().filter(|h| h.wants_entity_touch()) {
            h.entity_touch(b, t);
        }
    });
}

/// Version 6: player `e` fired a shot (from inside its PostThink, so deferred).
pub fn weapon_fired(e: *mut edict_t, weapon: i32, origin: [f32; 3], dir: [f32; 3], silenced: bool) {
    if any() {
        let i = index(e);
        after(&[e], move || each(|h| h.weapon_fired(i, weapon, origin, dir, silenced)));
    }
}

/// Version 6: player `e` picked up `item` (`kind` an `ts_plugin_api::entity` kind, `id` its weapon
/// id or power-up bits), deferred until the player is back in its slot.
pub fn item_pickup(e: *mut edict_t, item: *mut edict_t, kind: i32, id: i32) {
    if any() {
        let (i, it) = (index(e), index(item));
        after(&[e], move || each(|h| h.item_pickup(i, it, kind, id)));
    }
}

/// A string from a client for a C callback (its arguments have no NULs; one that did would be "").
fn c_string(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_default()
}

/// Version 8: player `e`'s client answered `tspkg <id> <version> <state> [reason]` (from its
/// command, so at once).
pub fn client_plugin_state(e: *mut edict_t, id: &str, version: &str, state: i32, reason: &str) {
    if any() {
        let (i, id, version, reason) = (index(e), c_string(id), c_string(version), c_string(reason));
        each(|h| h.client_plugin_state(i, &id, &version, state, &reason));
    }
}

/// Version 8: player `e`'s `tspkg list` is complete: its loaded plugins (id, version).
pub fn client_plugin_list(e: *mut edict_t, plugins: &[(String, String)]) {
    if any() {
        let i = index(e);
        let strings: Vec<_> = plugins.iter().map(|(id, v)| (c_string(id), c_string(v))).collect();
        let list = client_plugin_entries(&strings);
        each(|h| h.client_plugin_list(i, &list));
    }
}

/// The C entries of [`client_plugin_list`], pointing into `strings` (which must outlive them).
fn client_plugin_entries(strings: &[(std::ffi::CString, std::ffi::CString)]) -> Vec<ts_plugin_api::ClientPlugin> {
    strings.iter().map(|(id, v)| ts_plugin_api::ClientPlugin { id: id.as_ptr(), version: v.as_ptr() }).collect()
}

/// Whether anything listens for [`melee_attack`].
pub fn wants_melee_attack() -> bool {
    all().iter().any(|h| h.wants_melee_attack())
}

/// Version 5: a kung-fu move of `player` is about to land. Each plugin may change `time` and
/// `damage` (a value that isn't finite or is below 0 is ignored); the first that blocks stops it
/// and the rest aren't asked. True: blocked. Runs at once: the caller lends the player.
pub fn melee_attack(player: i32, time: &mut f32, damage: &mut f32) -> bool {
    melee_chain(&all(), player, time, damage)
}

fn melee_chain(hooks: &[Rc<dyn PluginHooks>], player: i32, time: &mut f32, damage: &mut f32) -> bool {
    for h in hooks.iter().filter(|h| h.wants_melee_attack()) {
        let (mut t, mut d) = (*time, *damage);
        let block = h.melee_attack(player, &mut t, &mut d);
        if t.is_finite() && t >= 0.0 {
            *time = t;
        }
        if d.is_finite() && d >= 0.0 {
            *damage = d;
        }
        if block {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A plugin that scales the time and damage and maybe blocks, counting its calls.
    struct Melee {
        time: f32,
        damage: f32,
        block: bool,
        calls: Cell<i32>,
    }

    impl PluginHooks for Melee {
        fn wants_melee_attack(&self) -> bool {
            true
        }
        fn melee_attack(&self, _player: i32, time: &mut f32, damage: &mut f32) -> bool {
            self.calls.set(self.calls.get() + 1);
            *time *= self.time;
            *damage *= self.damage;
            self.block
        }
    }

    struct Quiet;
    impl PluginHooks for Quiet {}

    fn melee(time: f32, damage: f32, block: bool) -> Rc<Melee> {
        Rc::new(Melee { time, damage, block, calls: Cell::new(0) })
    }

    #[test]
    fn melee_attack_chains_and_the_first_block_stops() {
        let (a, b, c) = (melee(2.0, 0.5, false), melee(1.0, 3.0, true), melee(1.0, 1.0, false));
        let hooks: Vec<Rc<dyn PluginHooks>> = vec![Rc::new(Quiet), a.clone(), b.clone(), c.clone()];
        let (mut t, mut d) = (0.35, 20.0);
        assert!(melee_chain(&hooks, 1, &mut t, &mut d));
        assert_eq!((t, d), (0.7, 30.0), "each sees the last one's values");
        assert_eq!((a.calls.get(), b.calls.get(), c.calls.get()), (1, 1, 0));
        // Nobody blocks: the move goes on with the changes.
        let hooks: Vec<Rc<dyn PluginHooks>> = vec![a.clone()];
        let (mut t, mut d) = (0.5, 10.0);
        assert!(!melee_chain(&hooks, 1, &mut t, &mut d));
        assert_eq!((t, d), (1.0, 5.0));
    }

    #[test]
    fn client_plugin_entries_point_at_their_strings() {
        assert_eq!(c_string("web view"), c"web view".to_owned());
        assert_eq!(c_string("a\0b"), std::ffi::CString::default(), "a NUL can't come from argv; it would be dropped");
        let strings = [(c_string("webview"), c_string("0.1.0")), (c_string("manual:dev"), c_string("0.0.0"))];
        let list = client_plugin_entries(&strings);
        assert_eq!(list.len(), 2);
        assert_eq!((list[1].id, list[1].version), (strings[1].0.as_ptr(), strings[1].1.as_ptr()));
        assert!(client_plugin_entries(&[]).is_empty());
    }

    #[test]
    fn melee_attack_ignores_bad_values() {
        let hooks: Vec<Rc<dyn PluginHooks>> = vec![melee(f32::NAN, -1.0, false), melee(f32::INFINITY, 1.0, false)];
        let (mut t, mut d) = (0.35, 20.0);
        assert!(!melee_chain(&hooks, 1, &mut t, &mut d));
        assert_eq!((t, d), (0.35, 20.0));
    }

    #[test]
    fn listeners_get_handles_and_leave() {
        let mut l = Listeners { next: 1, list: Vec::new() };
        let h = l.add(0, Rc::new(Quiet));
        let h2 = l.add(0, melee(1.0, 1.0, true));
        assert!(h > 0 && h2 > 0 && h != h2);
        assert!(l.remove(h2));
        assert!(!l.remove(h2), "already gone");
        assert!(!l.remove(99));
        assert_eq!(l.list.len(), 1);
        let h3 = l.add(0, Rc::new(Quiet));
        assert!(h3 != h && h3 != h2, "handles aren't reused");
        // After i32::MAX the count starts over at 1, never 0 or below.
        l.next = i32::MAX;
        assert_eq!(l.add(0, Rc::new(Quiet)), i32::MAX);
        assert_eq!(l.next, 1);
    }

    #[test]
    fn a_detach_drops_the_owners_listeners_only() {
        let mut l = Listeners { next: 1, list: Vec::new() };
        let mine = l.add(7, Rc::new(Quiet));
        let anon = l.add(0, Rc::new(Quiet));
        let other = l.add(8, Rc::new(Quiet));
        l.remove_owned(7);
        let left: Vec<i32> = l.list.iter().map(|x| x.handle).collect();
        assert_eq!(left, [anon, other]);
        assert!(!l.remove(mine), "gone with its owner");
    }

    struct Bots(&'static str);
    impl crate::bot_backend::BotBackend for Bots {
        fn name(&self) -> &str {
            self.0
        }
        fn add_bot(&self, _req: &crate::bot_backend::AddBot) -> Option<(*mut edict_t, Option<f32>)> {
            None
        }
    }

    /// Version 7 (#290), through the registries themselves (the only test that touches them, so
    /// the test threads don't share them): an attached plugin has a name, a data directory and a
    /// bot backend, owns listeners, and a detach takes all of it back.
    #[test]
    fn an_attached_plugin_owns_its_dir_backend_and_listeners_until_it_detaches() {
        use crate::bot_backend;
        use ts_plugin_api::{EXISTS, INVALID, NO_PLUGIN};
        assert_eq!(attach_plugin("a b"), INVALID);
        assert_eq!(attach_plugin(""), INVALID);
        let id = attach_plugin("rcbot_amxx");
        assert!(id > 0);
        assert_eq!(attach_plugin("RCBOT_AMXX"), EXISTS);
        assert_eq!(reserve("rcbot_amxx"), None, "a tsmod_rcbot_amxx library isn't loaded then");
        assert!(is_attached(id) && !is_attached(NO_PLUGIN));
        assert_eq!(name_of(id).as_deref(), Some("rcbot_amxx"));
        assert_eq!(host::data_dir_of("ts", &name_of(id).unwrap()), std::path::Path::new("ts/tsmod/rcbot_amxx"));
        assert_eq!(host::prefix(id), "[rcbot_amxx]");
        assert!(names().is_empty(), "not a loaded plugin: no descriptor");
        assert!(any(), "the frame is built for it");

        // Its backend, registered before `GameDLLInit`'s native one: native still comes first.
        assert_eq!(host::check_backend(id, "rcbot_amxx"), Ok(()));
        assert_eq!(host::check_backend(NO_PLUGIN, "rcbot_amxx"), Err(INVALID));
        assert!(bot_backend::register_plugin(id, Rc::new(Bots("rcbot_amxx"))));
        assert!(!bot_backend::register_plugin(id, Rc::new(Bots("RCBot_AMXX"))), "taken");
        assert!(bot_backend::register(Rc::new(crate::bots::backend::Native)));
        assert_eq!(bot_backend::active().name(), bot_backend::NATIVE, "the default is native, not the first registered");
        assert!(bot_backend::is_registered("rcbot_amxx"));

        let mine = add_listener(id, Rc::new(Quiet)).unwrap();
        let anon = add_listener(NO_PLUGIN, Rc::new(Quiet)).unwrap();
        assert_eq!(add_listener(id + 100, Rc::new(Quiet)), None, "not attached");

        assert!(detach_plugin(id));
        assert!(!detach_plugin(id), "once");
        assert!(!detach_plugin(NO_PLUGIN));
        assert!(!is_attached(id) && name_of(id).is_none());
        assert_eq!(host::prefix(id), "[plugin]");
        assert_eq!(host::check_backend(id, "rcbot_amxx"), Err(INVALID), "a dead id registers nothing");
        assert!(!bot_backend::is_registered("rcbot_amxx"), "its backend went with it");
        assert!(bot_backend::is_registered(bot_backend::NATIVE));
        assert!(!remove_listener(mine), "its listener went with it");
        assert!(remove_listener(anon), "others' stay");
        assert!(!any());

        let again = attach_plugin("rcbot_amxx");
        assert!(again > id, "a new id for the same name");
        assert!(detach_plugin(again));
    }

    /// #291: a failed load's id isn't handed out again, and names are one plugin's.
    #[test]
    fn ids_are_never_reused_and_names_are_unique() {
        let mut r = Registry { next: 1, list: Vec::new() };
        let a = r.add("a", false).unwrap();
        assert!(r.remove(a, false), "the load failed");
        let b = r.add("b", false).unwrap();
        assert_ne!(a, b, "the failed plugin may still call with its id");
        let c = r.add("rcbot_amxx", true).unwrap();
        assert!(c > b);
        assert_eq!(r.add("RCBot_AMXX", false), None, "an attached name keeps a library out");
        assert_eq!(r.add("B", true), None, "a loaded name can't be attached");
        assert!(!r.remove(c, false), "not a loaded plugin");
        assert!(!r.remove(b, true), "not an attached plugin");
        assert!(r.remove(c, true), "detached");
        let again = r.add("rcbot_amxx", true).unwrap();
        assert!(again > c, "the name is free again, with a new id");
        r.next = PluginId::MAX;
        assert_eq!(r.add("last", true), None, "out of ids rather than wrapping to a used one");
    }
}
