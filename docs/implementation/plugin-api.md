# Server plugin API

TS 3.0 kept player state in its C++ `CBasePlayer`. AMX Mod X's TS module (`tsx`, source in
`amxmodx/modules/ts/tsx`, `NBase.cpp`, `CMisc.h`) read and wrote it at fixed int offsets, and
plugins used fakemeta's `get_pdata_*` / `set_pdata_*` the same way. Those offsets were for TS 2.x
and are wrong for 3.0 anyway. The port keeps the state in Rust objects (`player::Player`,
`rules::PlayerRules`, `weapons::Inventory`), and an edict's private data holds only a slab handle
(design.md §4.2). So the port adds an API that TS doesn't have: a C table of functions. Two kinds
of plugin use it:

* **metamod plugins** get it from `CreateInterface`: the AMX Mod X modules in
  `the-specialists-mod/tsmod-amxx-rs`;
* **`tsmod_*` libraries** that the server itself loads from its `dlls/` directory (§1.2) get it
  from their `TSRS_PluginLoad` and hand back callbacks. Bot backends are such plugins
  ([bot-plugins.md](bot-plugins.md)).

The current version is **8** (#294, #304 and #321: `TSExt` messages to the port client's
extensions, the port client's version, and the client plugin offers; version 7 was #290: plugin identities for metamod plugins, so an AMX Mod
X-hosted bot can be a backend; version 6 was tsmod-rcbot-rs's needs, version 5 tsmod-amxx-rs#8
and #10, version 4 #117 and #99, version 3 #104). Versions don't stay compatible: a break bumps
`VERSION` and the interface name, so a stale plugin finds nothing (§8, §9, §10, §11, §12, §13).

| Part | Where |
|---|---|
| The table, constants and contract (append-only, `no_std`) | `crates/plugin-api` (`ts-plugin-api`) |
| `CreateInterface` and the table's `extern "C"` functions | `crates/server-dll/src/exports.rs`, `plugin_api.rs` |
| The logic: argument checks, deferral, snapshots | `crates/server/src/plugin_api.rs` (tests in `plugin_api/tests.rs`); bots `plugin_api/bots.rs`; entities and game state `plugin_api/world_info.rs` |
| The `tsmod_*` loader: naming rule, header check, descriptor checks | `crates/server/src/plugins/loader.rs` (pure, unit tested; the naming and header rules are `crates/shared/src/plugin_lib.rs`, shared with the client's extension loader, client-ext-api.md); opening the library and calling it: `crates/server-dll/src/plugins.rs` |
| Plugin callbacks (when they run), log, data directory, commands, cvars | `crates/server/src/plugins/mod.rs`, `plugins/host.rs` |
| `ext_message` and `client_port_version` (version 8, §13) | `crates/server/src/plugin_api/ext.rs` (the `TSExt` wire format `crates/shared/src/protocol/ext.rs`, client-ext-api.md §10) |
| `offer_client_plugin`, `client_plugin_version`, the `client_plugin_state` and `client_plugin_list` events (version 8, §13) | `crates/server/src/plugin_api/client_plugins.rs`; the offers and each client's answers and list `crates/server/src/plugin_offers/` (`mod.rs`: `offer`, `loaded_version`, the events from `client_command`; `clients.rs`: `Client::loaded_version`); the events' dispatch `plugins/mod.rs` |
| Plugin ids, attached plugins and listener owners (version 7, §1.4) | `crates/server/src/plugins/mod.rs` (`Registry`, `attach_plugin`, `detach_plugin`); their backends `crates/server/src/bot_backend/mod.rs` (`register_plugin`, `drop_plugin`) |
| The stub plugin | `crates/tsmod-test` (`cargo xtask plugins`) |
| Power-up runs, time scale, `TSSlowMo` override | `crates/server/src/rules/powerup.rs` |
| Free slots, weapon ammo | `crates/server/src/weapons/mod.rs` |
| Projectile throwers | `crates/server/src/weapons/projectiles.rs` (`Throwers`) |
| Private-data guard | `crates/goldsrc/src/edict.rs`, `crates/server/src/world.rs` (`private_size`) |
| Private-data mirror (version 5, §4.1) | `crates/server/src/pdata.rs` (layouts, sync, take-back; tests in it); block access `goldsrc::edict::{read,write}_private_u32` |
| Listeners and the stunt, power-up and melee-attack callbacks (version 5) | `crates/server/src/plugins/mod.rs`; the stunts `stunt/moves.rs` (`stunted`), power-ups `rules/powerup.rs`, melee `shared::weapons::GunHost::melee_attack_pre` (`Gun::ccombat_attack`: `crates/ts30/src/melee.rs`, `crates/ts21/src/shared/melee.rs`) and `weapons/host.rs` |
| The frame's world (version 6): `players`, `frame_info`, `items`, the inventory serial, `CombatState` | `crates/server/src/plugin_api/frame.rs` (built by `plugins::start_frame`); the stunt timers' end times `stunt::rules::StuntClock::ends_at` (2.1's in `crates/ts21/server/src/stunt.rs`) |
| `run_cmd`'s view, `msec`, result and events (version 6) | `crates/server/src/plugin_api/bots.rs`; the globals put back `goldsrc::server::Engine::set_frame_times` |
| The damage event, a sound's responsible player (version 6) | `crates/server/src/plugin_api/events.rs` (the hit from `Player::trace_attack`, the weapon `rules::scoring::blow_weapon`); the touch, shot and pickup callbacks `plugins/mod.rs`, called from `ham::touch`, `weapons/host.rs` (`shoot`), `entities/ts/ground.rs` (`get_weapon`), `entities/ts/powerup.rs` (`touch`), `weapons/worldgun.rs` (`give_to`) |
| `entity_key` (version 6) | `crates/server/src/plugin_api/keys.rs`, recorded by `entities::dispatch_key_value`, forgotten by `ham::on_free_private_data` |
| The scored kill (`kill_scored`, version 5) | the shared report `crates/server/src/rules/scoring.rs` (`kill_report`, `KillFlags`, `report_kill`); each ruleset's `PlayerKilled` fills in its points, sprees and tags: `crates/ts30/server/src/scoring.rs`, `crates/ts21/server/src/scoring.rs` |
| Entity hooks (version 2, §7): numbering, marshalling, pre/post calls, `ham_execute` | `crates/server/src/ham/` (`mod.rs`, `table.rs`, `classes.rs`, `names.rs`; queries `query.rs`, the gun `gun.rs`; tests in `tests.rs`) |
| The gun's hookable virtual calls (`Item_*` / `Weapon_*`) | `shared::weapons::Virtual`, `GunHost::virtual_pre` / `virtual_post` (`crates/shared/src/weapons/gun.rs`); the server's in `crates/server/src/weapons/host.rs` |
| Calling the plugin's hook (FFI), `ham_execute`'s pointer arguments | `crates/server-dll/src/plugin_api.rs` (`call_hook`, `ham_execute`) |

## 1. Getting the table

### 1.1 Metamod plugins: `CreateInterface`

The server library exports tier1's `CreateInterface(name, return_code)` on every platform.
`mp.dll` always had it, with nothing registered (0x10028f70). On Linux it is the one export the
port adds to `ts_i386.so`'s list (`stock-entities.md` §7.5). With `TSRS_PluginApi008`
(`ts_plugin_api::INTERFACE_NAME`) it returns a pointer to the `PluginApi` and writes 0
(`IFACE_OK`) to a non-NULL `return_code`. Any other name, the earlier versions' `TSRS_PluginApi001`
to `TSRS_PluginApi007` included, or a NULL name, returns NULL and writes 1 (`IFACE_FAILED`).

A plugin looks the export up in the game library that is already loaded: metamod's
`GINFO_REALDLL_FULLPATH`, then `dlopen(RTLD_NOLOAD)` / `GetModuleHandle`. It checks
`PluginApi::version` (8) and `size`. Such a plugin has no plugin id of its own: it passes
`NO_PLUGIN` (0) where one is asked, and then can't use `data_dir`, `register_bot_backend` or
`create_bot`, unless it takes an identity with `attach_plugin` (version 7, §1.4). Since version 5
it can register a `Plugin` descriptor as a **listener** (`add_listener`, §1.3) to get the
callbacks a loaded plugin gets.

The table is built on first use after `GiveFnptrsToDll` (it carries the engine's table and
globals pointers, `engine_funcs` / `globals`).

### 1.2 `tsmod_*` libraries: the loader

At `GameDLLInit`, after the game's own init, the server loads plugins from `<gamedir>/dlls/`
(the engine's game directory, where the server library itself is). Once per process; libraries
are never unloaded. The rule (`plugins/loader.rs`):

* The file name is **`tsmod_<name>` + the server library's platform suffix**
  (`ts_plugin_api::library_suffix`):

  | Server library | Plugin |
  |---|---|
  | `mp.dll` (Windows i686) | `tsmod_<name>.dll` |
  | `mp_amd64.dll` (Windows x86-64) | `tsmod_<name>_amd64.dll` |
  | `ts_i386.so` (Linux i686; stock HLDS loads it as `ts.so`) | `tsmod_<name>_i386.so` |
  | `ts_amd64.so` (Linux x86-64) | `tsmod_<name>_amd64.so` |

  `<name>` is 1-32 letters, digits, `_` or `-`. The longest matching suffix decides, so
  `tsmod_x_amd64.dll` is a win64 plugin, not a win32 one named `x_amd64`. Windows compares
  names case-insensitively. Files load in name order.
* A `tsmod_` file with **another platform's suffix** is skipped with a warning, without opening
  it. A file with this platform's name whose **ELF/PE header** says another machine (or isn't an
  image) is skipped with a warning before the system loader sees it.
* The library is opened and its export **`TSRS_PluginLoad(api, api_size, info, out, out_size)`**
  (`ts_plugin_api::LoadFn`) called with the table, a `LoadInfo` (the plugin id, `<name>`, the
  path) and a descriptor whose `size` is set and everything else zero. The plugin may call the
  API already (register cvars, commands, its bot backend). It fills the `Plugin` descriptor
  (name, version, callbacks) and returns `OK`.
* Skipped with a warning, never a crash: a library that can't be opened, has **no
  `TSRS_PluginLoad`**, returns anything but `OK`, or leaves a **descriptor `size`** other than
  the server's or an **`api_version`** other than `VERSION` (8). What a refused plugin registered is dropped
  (its commands then do nothing; its bot backend is gone). A library whose load function ran
  stays mapped.

Log lines: `[ts] plugins: loaded tsmod_rcbot_i386.so (RCBot 0.1)`, `[ts] plugins:
tsmod_x_amd64.so: built for linux-i386 (this server is linux-amd64), skipped`, `[ts] plugins:
tsmod_y.dll: has no TSRS_PluginLoad export, skipped`, ...

### 1.3 Plugin callbacks

All optional, on the engine's thread (`ts_plugin_api::Plugin`): `game_init` (after every plugin
loaded), `precache` (after the game's world precache), `server_activate(map)`,
`server_deactivate`, `start_frame` (after the bots, before the rules: plugin bots run here),
`shutdown` (`GameShutdown`), `client_connect` / `client_put_in_server` / `client_disconnect`
(with `is_bot`; bots made by any backend get `client_put_in_server` only, as TS's `BotSetup`
skips `ClientConnect`), `client_command` (before the game's commands; non-zero swallows it),
`player_spawned`, `damage` (version 6, below; it replaces version 5's `player_damaged`),
`player_killed`, `on_kill` (a kill a player scored, with its points), `round_start` /
`round_end` (last man standing), `sound` (every `EmitSound` / `EmitAmbientSound` the game plays,
through `goldsrc::server::set_sound_hook`; installed only when a plugin has the callback; version
6 adds the channel, `CHAN_STATIC` for an ambient sound, and the responsible player: the entity
itself when a player, else `projectile_owner`'s answer), `weapon_changed` / `ammo_changed` (from
the per-frame snapshot's weapon, clip and reserve, at most once a frame: TS sends bots no weapon
messages). Version 5 adds:

* `player_stunt(player, kind)`: a stunt started, `ts_plugin_api::stunt` kinds. 2 roll (`GoRoll`
  0x9cfe8, the roll out of a dive's `Landing` 0x9bbf4, a side roll out of `HandleSlide`
  0x9b9f8), 3 dive (`GoDive` 0x9b1a0, wall dives too, and the slow-motion dodge `GoBulletTime`
  0x9a5b4), 4 getting up from a slide (`HandleSlide`), 5 flip or cartwheel (`GoWheel` 0x9c9bc):
  `tsconst.inc`'s `STUNT_*` numbers. 6 wall jump (`GoWallJump` 0x9a7d4) and 7 wall climb
  (`GoWallClimb` 0x9aa1c) are the port's numbers for 3.0 stunts TS 2.x didn't have. TS 3.0's
  `TSState` only says stand / crouch / prone, so these come from the moves themselves.
* `player_powerup(player, bits)`: a power-up took effect: the held one was used (`UsePowerUp`
  0x95980, `usepowerup` or the bots), or a pickup that works at once (`PlayerAddPowerUp`
  0x97bc8: armour, kung fu, superjump, health, grenade, and infinite clip under rules that apply
  it at once). A held pickup (slow motion, slow pause, double fire rate, infinite clip) fires when
  used. The API's `run_powerup` doesn't fire it.
* `melee_attack(player, &time, &damage)`: before a kung-fu move lands (`CTSGun::CCombatAttack`
  0x86078, after the move is picked): `time` is its delay (the next attack, `pl+0x4f8`, and x0.75
  the `BlockPlayer` time), `damage` what a hit deals (`melee_damage` of the kung-fu power, doubled
  by the Kung Fu power-up). The plugin may change both (non-finite or negative values are
  ignored) and return non-zero to block the move: no hit, delayed-hit window, crouch,
  `BlockPlayer`, animation or event, but the delay still runs (so a held button doesn't ask every
  frame). Plugins run in order and the first block stops the chain. It runs at once, with the
  player lent to its slot as for a gun hook (§7.5), so the API reaches it. TS 2.1 mode asks it
  from 2.1's own `CCombatAttack` (0x1004de10) with the same semantics: `time` is the move's delay
  before 2.1's time-scale factor, which then applies to the answer (ts21.md, Scoring).
* `kill_scored(&KillInfo)` (#197): every death `PlayerKilled` (0x895e4, the 3.0 plugin's
  `scoring::player_killed`) scores, kills, suicides and deaths by the world alike, once the frags,
  `DeathMsg` and `ScoreInfo` went out. `KillInfo` carries the killer (the victim for a suicide, 0
  for no player), victim, inflictor, the frags the killer got (`points`, after the double kill,
  specialist and spree; -1 a team kill), the blow's TS weapon id (by the killer's own hand
  `own_weapon_name`'s choice as an id, 0 for kung fu; a thrown blade's id; 24 a grenade; else
  -1), the `DeathMsg` name, both sprees, and `ts_plugin_api::kill` bits: the log tags
  (`victims_weapon`, `stopped_fury`, `killed_specialist`, `double_kill`, `diving`, `sliding`,
  `killed_the_one`, the spree award), the kung-fu frag, and `glast_headshot` /
  `glast_closecombat` / `glast_coldcock`. It is deferred like `on_kill`, so a plugin gets it after
  the victim's `Killed` (its `TSHealth` 0 included), also when the scoring itself was deferred
  for a killer out of reach. A teamplay team change's death (`death_suppressed`) isn't scored and
  has none. TS 2.1 mode reports from its own `PlayerKilled` (0x1004f7e0) with 2.1's values
  (#198): its points, sprees and the tags it has (its spree awards at the same counts, its
  stunt styles, the kung-fu frag); `victims_weapon` and `HEADSHOT` are never set there, as 2.1
  has neither (ts21.md, Scoring). Both rulesets fill the shared part with
  `ts_server::rules::scoring::kill_report`.

Version 6 adds (for bot backends; all deferred until the players they name are back in their
slots):

* `damage(&DamageEvent)`: a player's `TakeDamage` ran (`Player::take_damage`): victim, attacker,
  inflictor (-1 none), the damage asked for and its bits, the health and armour left, and what
  version 5 couldn't tell: the hitgroup, direction and position of the `TraceAttack` it came from
  (TS's `TakeDamage` gets four arguments only, so the server keeps the last `TraceAttack` on the
  player in this frame, `plugin_api::events`), and the blow's TS weapon id counted as
  `KillInfo::weapon` (`rules::scoring::blow_weapon`, from TS's `glast_*` globals).
* `entity_touch(player, other)`: `DispatchTouch` with a bot (`FL_FAKECLIENT`, any backend) on
  either side, after the entity's `Touch` ran (`ham::touch`): triggers, pickups, doors, the world,
  other players. A solid contact comes from both entities' touch, so it can come twice.
* `weapon_fired(player, weapon, origin, dir, silenced)`: every `ShootBullets` (each round of a
  burst; a shotgun's pellets are one call), from the gun's `shoot` (`weapons/host.rs`): the gun
  position, the unit aim before the spread, and whether a silencer is on (`Gun::silenced`).
  Melee, throws and grenades don't fire it.
* `item_pickup(player, entity, kind, id)`: a ground weapon taken by `use` (whatever the ruleset's
  `give_ground_weapon` decided, seen as a change of the inventory), a power-up (its bits, before a
  random one re-rolls) or a dropped gun (`CTSWorldGun::GetWeapon`), with the `entity_info` kind and
  the weapon id or power-up bits. A temporary power-up or dropped gun is removed by then.

Version 8 adds (#321, client-plugins.md §6.1, §13.3; both run at once, from the client's
`tspkg` command, after `client_command` saw it):

* `client_plugin_state(player, id, version, state, reason)`: every `tspkg <id> <version> <state>
  [reason]` answer the server takes (to an offer, a `block`'s `suspended`, op 5's `tsrs`); `state`
  is `ts_plugin_api::plugin_state` (0 `have` to 10 `restart`, the protocol's order), `reason` ""
  if none. Answers the server ignores (ids it didn't send, over the rate limit) don't fire it. For
  a refused required offer it fires before the kick (`sv_plugin_required_kick`).
* `client_plugin_list(player, plugins, count)`: the client's `tspkg list` is complete (its closing
  line), with `count` `ClientPlugin { id, version }` entries (`manual:<name>` and `0.0.0` for a
  hand-installed library). Once per connection, so at every map; a list still incomplete when the
  grace period ends doesn't fire it.

**Listeners** (version 5): `add_listener(plugin, &Plugin)` registers a descriptor for a plugin the
server didn't load (a metamod plugin, such as `tsmod-amxx-rs`'s tsx). The server checks its `size`
and `api_version`, copies it and calls it like a loaded plugin's, after the loaded plugins, in
registration order, until `remove_listener(handle)`. Events already past (`game_init`, a map's
`precache`) are called when they come round again. Version 7 adds the first argument, the
listener's owner: `NO_PLUGIN`, or an attached plugin's id (§1.4), whose `detach_plugin` removes it;
any other id is `INVALID`.

The player callbacks the game reaches inside a player's own dispatch (spawn, damage, death,
kill, stunt, power-up) are **deferred** until every player they name is back in its slot
(`world::defer`), so a callback can use the whole API on them. The others run at once; `sound`
can run inside another entity's call (the API's snapshot rules apply).

### 1.4 Attached plugins (version 7)

A metamod plugin (an AMX Mod X module, at `Meta_Attach` / `AMXX_Attach`) takes a plugin identity
with `attach_plugin(name)`, so it can own a data directory and a bot backend as a `tsmod_*`
library does. This is what lets an AMX Mod X-hosted bot (a Pawn plugin on Rust AMXX modules,
`the-specialists-mod/tsmod-amxx-rcbot`) be `ts_bot_backend rcbot_amxx`.

* **The name** follows `tsmod_<name>`'s rule (`loader::valid_name`: 1 to 32 letters, digits, `_`,
  `-`). `INVALID` for NULL, a bad name or one that isn't UTF-8; `EXISTS` when a loaded or attached
  plugin has it, letter case ignored (the data directory's name; Windows file names). A `tsmod_*`
  library whose name an attached plugin took first is skipped with a warning ("the plugin name
  ... is taken"), as metamod plugins attach before `GameDLLInit` loads the libraries.
* **The id** (> 0) comes from the same counter as loaded plugins' and is never handed out again
  (#291), also after a detach or a failed load. It works wherever a loaded plugin's does:
  `data_dir` (`<gamedir>/tsmod/<name>/`, the same scheme, `host::data_dir_of`), `log` (prefix
  `[<name>]`, a loaded plugin's being `[tsmod_<name>]`), `register_command`, `register_cvar`,
  `register_bot_backend`, `create_bot` (when its backend is the active one), `add_listener`'s owner.
* **When**: any time on the engine's thread, also before `GameDLLInit` (metamod calls the game's
  `GiveFnptrsToDll` before it attaches its plugins, so the engine's functions are there). A
  backend registered that early doesn't become the default: native is put first whenever it
  registers (`bot_backend::add`).
* **`detach_plugin(id)`** gives the identity up: `OK`, or `INVALID` for an id that isn't attached
  (`NO_PLUGIN`, a loaded plugin's, which stay for the process, or one already detached). Its server
  commands do nothing from then on (the engine keeps the names, as for a failed load); its
  listeners are removed; its bot backend is unregistered, and if it was the active one it is
  deactivated, the bots it made are kicked and native is activated at once, as a map start
  switching backends (`bot_backend::drop_plugin`). `ts_bot_backend` may still name it: that then
  resolves to native (warned once), so the map isn't reloaded for it. Its cvars stay (the engine
  can't drop one) and so does an entity hook it installed (`ham_set_hook` is server-wide). The
  name is free again; a new attach gets a new id.

## 2. Rules: snapshots and deferral

Plugins run inside engine callbacks. A plugin can run while the server is inside one of that same
player's callbacks: a message hook (`register_message`, `register_event`) during the player's
PreThink or `UpdateClientData`, or a forward during its damage. The player object is then taken
out of the slab (`world.rs` take-out dispatch), so:

* **`player_info`** reads the object if it can be reached (in its slot, or lent back,
  `world::with_lent`). Otherwise it returns the last snapshot. A snapshot belongs to a user id,
  so a slot's next client never sees the previous client's. If a player can't be reached and has
  no snapshot yet (inside its first spawn), only `time_scale` (`pev->fuser1`) is filled.
  Snapshots are refreshed:
  * at the end of every `PlayerPostThink` (`Player::post_think`),
  * every `StartFrame`, for every player that can be reached (`plugin_api::start_frame`, after
    the bots and the rules),
  * after every change a setter makes, and
  * whenever `player_info` reads a live object.
* **Setters** check the player first (`NOT_PLAYER`: not a connected player, slot outside
  1..`maxClients`), then their arguments (`INVALID`; nothing changes). Then they make the change
  at once if the object can be reached: `OK`. That includes a player lent to its slot, which
  `world::run_now` opens, as TS changes a player inside its own call. Otherwise they queue it
  with `world::defer` and return `DEFERRED`. The queued change runs when the holding dispatch
  returns, and only if the slot still holds a connected player.
* Messages: only `set_award` (`TSPAward`) and `run_powerup` (`PwUpTime`, `TSMBlur`) send messages
  during the plugin's call, as TS's own paths do. Everything else goes out with the player's next
  `UpdateClientData` (`player::hud`, `weapons::net`), through TS's "on change" caches.

## 3. The functions

Addresses are `ts_i386.so` (Ghidra numbering). "pl+" offsets are `CBasePlayer` fields
(player-movement.md §8, gamerules-entities.md §2.2-2.3, weapons.md).

| Function | TS state | Port state and behaviour |
|---|---|---|
| `player_info` | `joined`: past the menus. `cash` pl+0x540. `free_slots` pl+0x548 (`TSSpace`). `award` pl+0x74c (`TSPAward`). `kill_streak` pl+0x744. `active_powerup` pl+0x724, `powerup_until` pl+0x72c. `stored_powerup` / `_value` pl+0x728 / pl+0x730 (`PwUp`). `kungfu` pl+0x74e, `superjump` pl+0x74f. `time_scale` `pev->fuser1`. `stunt_state` pl+0x498 (`TSState`). The weapon fields are what `WeaponInfo` carries (`CTSGun::UpdateClientData` 0x858c8). | `Player::{joined, cash, active_powerup, powerup_until, kungfu, superjump}`, `inv.sv.free_slots`, `ts.{award, spree, held, held_secs}`, `stunt.stance`, `inv.gun` (current id, record clip/mode/owned attachments, reserve of its ammo type). With kung fu: id 0, `clip` = the kung-fu bar as `WeaponInfo` sends it, the rest 0. `powerup_until` is 0 when nothing runs. While a used power-up runs, `stored_powerup` is still that power-up, as in TS (pl+0x728 holds it until it ends). |
| `set_cash` | pl+0x540. `CBasePlayer::GiveCash` (0x9310c) clamps to 0..30000. `TSCash` from `UpdateClientData` (0x91d00) on change. | 0..30000, else `INVALID`. Sets `Player::cash`. `player::hud` sends `TSCash`. |
| `set_free_slots` | pl+0x548. `GetSpeedBySlots` (0x76bd4) and `SetPlayerSpeed` (0x93068). `TSSpace` on change. | 0..81 (81 is an empty inventory, `TSInit`). `weapons::set_free_slots`: the free weight, then `inventory::sync_speed` (walking speed, `m_flPlayerSpeed`, `pfnSetClientMaxspeed`) as after a pickup. The weapons carried don't change, so later pickups and drops count from the new value. |
| `set_award` | pl+0x74c, broadcast as `TSPAward` (MSG_ALL) by `PlayerKilled` (0x895e4) and `UpdateClientData` on change. | 0..255 (the byte). Sets `ts.award`, sends `TSPAward` at once (as scoring does) and updates the HUD cache so it isn't sent twice. TS's own code reads the byte too (the spree awards, LMS's 1/11), so a plugin's value takes part in those. |
| `run_powerup` | `UsePowerUp` (0x95980) and `UpdatePowerUpState` (0x95748): pl+0x724 active, pl+0x72c end. Base durations from `SetupPowerUp` (0x98014). | One bit of slow motion 1, infinite clip 2, slow pause 8, double fire rate 0x10 or superjump 0x100 (`powerup::RUNNABLE`: the bits TS reads while running, `InfiniteClips` 0x75fa8.., the 0.15 gravity). Anything else is `INVALID`. Negative duration: the base length with `pwupmult` 1 (3 s, 3 s, 2 s, 3 s; superjump has none, so `INVALID`). 0 duration or not finite: `INVALID`. Runs it as `UsePowerUp` does: active bits, end time, `PwUpTime`, and `TSMBlur` 1/2 for the slow ones. The held power-up isn't touched (`ts.plugin_powerup`; see Deviations). `powerup` 0 stops the running power-up at the next PreThink, through TS's expiry path. |
| `set_speed` | tsx's `ts_set_speed` wrote TS 2.x slow-motion fields. In 3.0 a time scale for others comes from activators: pl+0x17c self / pl+0x180 others, `SlowMotionFlow` (0x96bb0) every 0.5 s, `DeactivateSlowMotionFlow` (0x96a9c). | `speed` 0..2 (0 means frozen, floored to TS's 0.01 `FixedSlowAmt`, 0x990bc), `aura` ≥ 0 and `duration` ≥ 0, all finite, else `INVALID`. The player becomes an activator for `duration` s (`ts.plugin_speed`, `powerup::plugin_speed_think`): self and others at `speed`. `SlowMotionFlow` slows the players and projectiles within `aura` units that it can also see. On expiry or death everything goes back to 1 as `DeactivateSlowMotionFlow` does. Duration 0 ends a running one. It wins over a slow-motion power-up's amounts while it lasts. |
| `set_physics_speed` | tsx's `ts_set_physics_speed` (TS 2.x). 3.0's client eases its cosmetic clock (particles, sound pitch, tint) toward the `TSSlowMo` amount (client `0x1004bf10`; `UpdateClientData` sends 0.225 / 0.35 / pl+0x170). | Speed up to 2 (NaN or above 2: `INVALID`). It overrides the `TSSlowMo` amount (`ts.physics_speed`, `powerup::hud_slow_amount`). 0 or below pauses: 0.01, because the client reads 0 as 1. Exactly 1 hands the amount back to TS. The override lasts until the next spawn (`TSInit` resets `PlayerRules`). |
| `set_bullet_trail` | None in 3.0 (see Deviations). | `UNSUPPORTED` (after the `NOT_PLAYER` check). |
| `set_weapon_ammo` | `CTSGun` records (`s_weapon_status`, gun+0xd4 reserve per ammo type). `WStatus` / `WeaponInfo` from `CTSGun::UpdateClientData` (0x858c8). | Weapon 1..37 with a table entry (not 29), clip 0..255, reserve 0..32767. Negative leaves that count alone. Out of range: `INVALID`. Not owned, or a reserve for a weapon without an ammo type (blades, grenade): `INVALID` when it runs (`OK` path) or nothing (deferred path). `weapons::set_weapon_ammo`: sets the clip with the akimbo split and the single/akimbo twin as a reload's finish does, and the reserve of the weapon's ammo type (shared with every weapon of that type, as in TS). Then the client gets `WStatus` for each changed record and `WeaponInfo` if the weapon is in hand, on the next update. |
| `projectile_owner` | Grenade `pev->owner`, thrown knife `CTSKnife+0x8c` (`CreateKnife` 0x7d9e4), thrown gun `CTSWorldGun` (dropper). | `weapons::projectiles::thrower_of`: the thrower of a grenade, knife or thrown gun, from a table filled when they are created (by edict index and serial number, so it answers while the projectile's own call is on the stack). The answer is 0 once the thrower has left or the slot holds someone else (`Thrower::get`). Any other game entity: its `pev->owner` if that is a connected player (a held `weapon_tsgun`), else 0. `NOT_PLAYER` for an index that is out of range, free, or has no game private data. |

### 3.1 Version 3: host, players, entities, game, bots

| Function | Port state and behaviour |
|---|---|
| `engine_funcs`, `globals` | The engine's `enginefuncs_t` (the FWGS headers' layout; the server's copy, don't write it) and `globalvars_t`. Traces, PVS, beams, menus, entity searches go straight to the engine. |
| `log(plugin, level, msg)` | `[tsmod_<name>] msg` on the console (an attached plugin's `[<name>]`, `NO_PLUGIN`'s `[plugin]`); `warning:` / `error:` tags; debug only with `developer`. |
| `data_dir(plugin, buf, len)` | `<gamedir>/tsmod/<name>/` (trailing separator), created on first use, for loaded and attached plugins. `INVALID` for `NO_PLUGIN` or a dead id. Strings out: NUL-terminated, cut to fit, the full length returned. |
| `map_name(buf, len)` | `gpGlobals->mapname`. |
| `register_command(plugin, name, f)` | A server command; `f(argc, argv)`. One trampoline is registered with the engine per name and finds the callback by `argv[0]`. `EXISTS` if a plugin has it; the engine warns about its own names. |
| `register_cvar(plugin, name, value, flags)` | `pfnCVarRegister` of a leaked `cvar_t`, or the existing one; returns the `cvar_t *`. The existing one is found with `Engine::cvar_lookup`, quiet on FWGS, whose `pfnCVarGetPointer` warns for every new name (#331). |
| `register_bot_backend(plugin, name, backend)` | Only plugins with an id (loaded; attached since version 7); a plugin name, not `native`; `EXISTS` if taken. One registered after the map start chose the map's backend takes over at once when `ts_bot_backend` names it and native runs. [bot-plugins.md](bot-plugins.md) §2.1. |
| `player_info` | As in §3, plus `alive` (alive and not observing), `is_bot` (`m_bIsBot`, any backend), `team` (the rules team from 1 in teamplay, 0 none) and `has_flag` (carries the CTB case). |
| `player_inventory(player, out, max)` | Every owned weapon id 1..37: clip, the reserve of its ammo type, fire mode, owned attachments. Returns the count. Snapshot like `player_info` when the player is out of reach. |
| `entity_info(entity, out)` | By the object's Rust type (`world::type_of`): player (team, `available` = alive), `GROUND_WEAPON` (`ts_groundweapon` and the HL names: TS id), `POWERUP` (bits), `GUN` (a dropped `WorldGun` with its id and dropper/thrower, or a `weapon_tsgun` and its holder, and since version 4 the holder's weapon in hand as `weapon`), `CTB_FLAG` (`ts_flag`, its team), `CTB_BASE` (`ts_briefcase`: capture team, `available` = captures accepted), `OBJECTIVE` (`ts_wingiver`, `ts_scoregiver`), `PROJECTILE` (thrower known), else `OTHER` (with `projectile_owner`'s owner). `available` is "not `EF_NODRAW`" unless said otherwise. This replaces RCBot's classname guesses and its `Classify()` virtual call. |
| `master_triggered(master, activator)` | `UTIL_IsMasterTriggered` by name (`entities::stock::base::is_master_triggered_name`): what RCBot did with `CMasterEntity::IsTriggered` through a vtable the port doesn't have. As in TS, a name that isn't a master counts as triggered (and logs "Master was null or not a master!"). |
| `game_state(out)` | `m_mode`, teamplay, LMS, The One, a CTB map (a `ts_briefcase` existed at `ServerActivate`; since version 6 found once per map, `world_info::map_started`, not on every call), team count, round state (`OVER` while `round_restart` is pending, `RUNNING` in a round mode with an end time), intermission, time, `maxClients`. Version 4: `team_scores[4]`, the `CTSGameRules` teams' scores that `TeamScore` carries (`TeamWin` 0x78a08, `CaptureTouch` 0x8db9c, `RecountTeams` 0xe5608; round wins and captures), by `PlayerInfo::team` - 1, 0 for an unnamed team. |
| `objectives(player, out, max)` | Version 4. The HUD objective list (`CTSGameRules +0x2d0`, filled by `CTSObjectivePointer::Use` 0x8bbd8) as `SendObjectiveToHud` (0x7ac80) sends it to `player` in the `Objective` message: `id` the list index (the message's byte), the entity, `origin` its `Center()`, `name` that player's team text cut to 63 bytes; plus `active` (a bomb, hack or escape zone enabled, not frozen; a win or score giver that hasn't fired). `player` 0 reads the texts as no team. Returns the count (emptied slots of TS 2.1's appending list skipped); `NOT_PLAYER` for any other index that isn't a connected player. |
| `create_entity(classname)` | Version 4. `CREATE_NAMED_ENTITY`; `INVALID` (and freed) for a class without a game object, as `CBaseEntity::Create` (0xac480) refuses one. Not spawned. |
| `entity_key_value(entity, key, value)` | Version 4. `DispatchKeyValue` (0xaad4c) with the entity's classname as `szClassName`: the entvars table (`origin`, `targetname`, ...) then the class (and its `Ham_Keyvalue` hooks). 1 handled, 0 not, `NOT_PLAYER`, `INVALID`. |
| `spawn_entity(entity)` | Version 4. `DispatchSpawn` (0xaab8c, with `Ham_Spawn`). A spawn that fails (-1: `FL_KILLME`, a refused class, a dead global) removes the entity, as the engine does with a map entity; one that removed itself is gone: both `INVALID`. RCBot's `UTIL_makeTSweapon` and `ExplosionCreate` are `create_entity` + keys + `spawn_entity` (+ `ham_execute(Ham_Use)`). |
| `create_bot(plugin, name, model, skill)` | Only for the active backend's plugin. The host's `BotSetup` (`bot_backend::create_player`): fake client, `Player` with `cbot`/`is_bot`, spawn, `model`/`team`/`cl_lw` keys, teamplay team from the model, `ts_playeramount`. Index, `FULL` or `INVALID`. `name` and `model` are NUL-terminated byte strings, not UTF-8: they become the netname, the keys and the team unconverted, as TS's `strcpy`s (`BotCreate` 0x1006b7c0 / `BotCreateCustom` 0x1006b590 in Windows `mp.dll`), so pass on `add_bot`'s strings as they came (a CP1252 0xE9 stays 0xE9; #190). |
| `kick_bot(player)` | `kick #<userid>` (queued). `INVALID` for a human. |
| `run_cmd(player, cmd, out)` | `pfnRunPlayerMove`. `BUSY` inside the bot's own callbacks; `INVALID` for a human, `msec` outside 0..255, an impulse outside 0..255, an unknown flag or a float that isn't finite. `ts_bot_stop 1` clears a living bot's movement, buttons and impulse (it still turns), as the native frozen think. Version 6 (`BotCmd::flags`): `SET_VIEW` writes `v_angle` and `angles` (pitch × -1/3, no roll) and clears `fixangle` before the move, as the native bots' `UpdateView` (0x1006e500) does. A fake client never gets the network update that clears `fixangle`, so without it a bot keeps its spawn's view for good. When the game had set `fixangle` (a spawn, a teleport, `ts_tp`), its angles (`pev->angles`, what `svc_setangle` sends a client) win for that command. `AUTO_MSEC` (or `msec` 0) runs `gpGlobals->frametime` in whole milliseconds, 1..255, carrying the fraction to the bot's next command. Afterwards the server puts `gpGlobals->time` and `frametime` back (FWGS's `SV_RunCmd` leaves them at `sv.time + frametime` and the command's length, `sv_pmove.c`) and fills `out` (`BotCmdResult`, may be NULL): `v_angle` (the game's while `fixangle` is set), origin, velocity, the `msec` used, and events `TELEPORTED` (moved farther than its speed allows: during the command, or between commands, when only a pusher moves a player), `FIXANGLE` (set before or during the command) and `LANDED` (in the air before, on the ground after). |
| `players(out, max)` | Version 6. Slot 1..`max` as `PlayerSnapshot`s; returns `maxClients`. Built once a frame at `StartFrame` before the plugins' `start_frame` (`plugin_api::frame::begin`), when every player is in its slot: the user id (0 empty), `player_flag` bits (joined, alive, bot, `FL_ONGROUND`, `FL_DUCKING`, ladder = `MOVETYPE_FLY`, water = `waterlevel` ≥ 2, the case, observing = `iuser1`, prone and stunting from the `iuser4` stunt bits; version 7: `KUNG_FU` and `SUPERJUMP`, the permanent power-ups `player_info` reports as `kungfu` / `superjump`; kung fu in hand is `weapon` 0), team, the entvars vectors, health, armour, buttons, `player_info`'s weapon, stance, power-up and time-scale fields, the raw stunt bits, frags, deaths, free slots, an inventory serial (+1 whenever `player_inventory`'s list changes) and a `CombatState`: the kung-fu bar (`pl+0x774`) and the game times at which the next attack (`m_flNextAttack` / the gun's primary delay, which a kung-fu move sets), the stunt cooldown, the recovery (`CTSStunt` or `BlockPlayer`), the prone lock, the slow-motion dodge, the kung-fu / knife / katana blocks (`pl+0x7a8..`) and the delayed hit (`pl+0x79c`) run out, 0 for none. TS 3.0 keeps those as seconds left of the player's time, TS 2.1 as end times; the ruleset's `StuntClock::ends_at` converts. |
| `items(out, max)` | Version 6. Every `ts_groundweapon` (and the HL names), `ts_powerup` and dropped gun (`CTSWorldGun`) in index order, taken ones too: index, serial number, `entity_info`'s kind, weapon, power-up bits and `available`, origin, and `respawn_at` (a hidden item's scheduled think; 0 when available, or when a round mode keeps it for `RespawnEntities`). Returns the count. |
| `frame_info(out)` | Version 6. The frame's time and frame time (at `StartFrame`), the highest entity index in use, and `game_state`'s teamplay, CTB, mode and round; version 7 its `team_scores` too (a bot's fitness reads them, which took a `game_state` call before). |
| `entity_key(entity, key, buf, len)` | Version 6. A map key the entity was given (by the map or `entity_key_value`) that the `entvars_t` table doesn't take, such as `master`, `wait` or `tsweaponid`: the port's classes parse those into their objects or drop them, so `entities::dispatch_key_value` records them by index and serial number (`plugin_api/keys.rs`) and freeing the entity forgets them. The last value given wins; an exact match before one ignoring ASCII case. The bytes as given. `UNSUPPORTED` for a key it wasn't given (an entvars key: read the edict), `NOT_PLAYER`, `INVALID` for a NULL key. RCBot fills its masters list from it. |
| `bot_command(player, line)` | `commands::run` with the line tokenized as the engine does (quotes group): plugins' `client_command`, then the game's commands, as if typed. `DEFERRED` when out of reach. |
| `select_weapon(player, id)` | `weapon_<id>` (`CmdStart` ignores `weaponselect` in TS 3.0, which is why RCBot never switched). `INVALID` if not owned. |
| `buy`, `buy_item`, `clear_loadout` | The loadout (`tkw`/`tka`, `tki`, `rst_cfg`: `AddItemToArmament`, `AddAmmoToArmament`), which `BuyArmament` applies at the next spawn under the server's restrictions and free slots. TS 3.0 charges nothing (`CashBased() == 0`). `akimbo` adds the second copy that makes the pair. |
| `drop_weapon(player, id)` | `drop` (negative id) / `drop_<id>`. |

## 4. The private-data guard

Plugins written for TS's object layout still use fakemeta's pdata natives, and AMX Mod X's
hamsandwich hooks entity vtables. A small block holding only the tag broke both
([#97](https://github.com/the-specialists-mod/the-specialistsmod-rs/issues/97)): offsets ran
past the end of the heap block, and on Windows hamsandwich followed the tag as a vtable pointer
and crashed the server. So every block the game allocates (`goldsrc::edict::set_private_tag`)
looks like this:

* At least `MIN_PRIVATE_SIZE` = 0x80 bytes. Every engine zeroes it (FWGS and ReHLDS `Mem_Calloc`,
  HLDS `calloc`).
* The slab tag (8 bytes) is at `PRIVATE_TAG_OFFSET` = 8. The vtable word, where hamsandwich's TS
  gamedata reads it (0 on Windows, 0x60 on Linux), stays zero.
* Since version 5, on 32-bit builds, `pev` at TS's place (`goldsrc::edict::PEV_OFFSET`: 4 on
  Windows, 0 on Linux, the gamedata's "pev"), so code that turns an object pointer into an
  entity (fakemeta's `get_pdata_ent`, hamsandwich's `get_pdata_cbase`) finds it. The block's size
  is recorded (`goldsrc::edict::private_size`, the API's `private_size`), and the game's word
  accesses (`read_private_u32` / `write_private_u32`) are bounded by it and never touch the tag.
* Players get `PLAYER_PRIVATE_SIZE` = 0x3000. That covers `CBasePlayer` (0xb8c in `ts_i386.so`,
  0xb78 in `mp.dll`) and `mp.dll`'s `CBot` (0x25f0, `GetClassPtr<CBot>` 0x1006c240), plus the
  5-int Linux shift.
* `weapon_tsgun` gets `TSGUN_PRIVATE_SIZE` = 0x1000. `CTSGun` is 0xa10 bytes, and `tsxaddon.inc`
  writes ints 41, 50-62, 839 and 850: byte 3420 with the Linux shift.
* Thrown knives get `KNIFE_PRIVATE_SIZE` = 0x100. `CTSKnife` is 0xa4 bytes, and the stock `tsx`
  module read int 31 (35 on Linux).

Outside the mirrored fields (§4.1) what plugins write there is **ignored by the game**, and
`get_pdata_*` reads 0 or whatever a plugin last wrote. Use the plugin API for TS state. Writes to
bytes 8..16 would break the entity's link to its object; no TS offset a plugin has a reason to
write lies there (it is `CBaseEntity`'s first members after `pev`/the vtable).

### 4.1 The mirror (version 5)

Plugins for TS 3.0 read a few members through fakemeta and hamsandwich (tsmod-amxx-rs#10; a
survey of `ref/ts-plugins`: `tsxaddon.inc`'s `ts_setuserammo` and its copies in `ARP_TS`,
`ARP_Items` and `ARP_NPCs` write the `weapon_tsgun`'s reserve per ammo type and its clip; `ARP_CS`
reads `m_pActiveItem` at Counter-Strike's offset, in code it disables). The server writes them
into the blocks at TS's offsets for its platform (`pdata::Layout::HOST`: `ts_i386.so`'s on
Linux, `mp.dll`'s on Windows, bots.md §14.2.1 / §14.3.1), and takes some writes back:

| Entity | Member | `ts_i386.so` | `mp.dll` (int) | Plugin writes |
|---|---|---|---|---|
| every block | `pev` (32-bit builds) | 0 | 4 (1) | |
| player | `m_pActiveItem`: its `weapon_tsgun`, a deployed `cycler_weapon`, else NULL (32-bit builds) | 0xa10 | 0x9fc (639) | overwritten |
| `weapon_tsgun` | `m_pPlayer` (32-bit builds) | 0x80 | 0x70 (28) | overwritten |
| | `m_iId` = 16 | 0x88 | 0x78 (30) | overwritten |
| | `m_iClip`: the gun in hand's clip | 0xb4 | 0xa4 (41) | **taken**: the current weapon's clip (not kung fu) |
| | current weapon id | 0xc8 | 0xb8 (46) | overwritten |
| | reserve of ammo type *t* (0..15) | 0xd4 + 4*t* | 0xc4 + 4*t* (49 + *t*) | **taken**, cut to 0..32767 |
| | weapon *id*'s record (0..37, 0x38 bytes): owned / ammo type / fire mode bytes | 0x114 + 0x38*id* | 0x104 + 0x38*id* | overwritten |
| | its clip | record + 4 | | **taken** for an owned weapon, cut to 0..255 (the akimbo split and twin as `set_weapon_ammo`) |
| | its owned / switched-on attachments | record + 0x10 / 0x14 | | overwritten |

`pdata::sync` takes the writes back (a word that differs from what the mirror last wrote, for
the same gun entity) and writes the mirror anew: at the end of the player's PostThink, at every
StartFrame (players in their slots), and around the API's setters. So reads are up to a frame
old, as `player_info`'s snapshot, and a write takes effect within a frame; the HUD follows on the
next update. Pointers aren't mirrored on 64-bit builds (no AMX Mod X there).

The offsets are TS's: a plugin's Linux shift must match. fakemeta's default `linuxdiff` of 5 ints
fits `CBasePlayer` (0xa10 - 0x9fc), but `CTSGun` is shifted by 4 ints (0x10). `tsxaddon.inc`
uses `mp.dll`'s offsets (reserve = int 49 + type; `m_iClip` = int 41) with the default shift, so
on Linux it lands one int off, as it did on TS's own Linux server; its ints 839 and 850 are past
the end of a 3.0 `CTSGun`. The `tsmod-amxx-rs` modules ship `ts_setuserammo` as a native instead.

**Stock hamsandwich is unsupported.** The port's entities have no C++ vtables. `RegisterHam`
fails cleanly with `[HAMSANDWICH] Failed to retrieve vtable for "player"` and a native error in
the plugin, and the server keeps running. Version 2 of the table offers the same hooks without
vtables (§7), for the `tsmod-amxx-rs` port of hamsandwich.

Save and restore only compare `pvPrivateData` pointers (`goldsrc::ext::saverestore`), so the
block's size and layout don't matter to them.

## 5. Deviations and limits

* The whole API is a port addition. TS 3.0 has no plugin interface.
* `set_bullet_trail` is `UNSUPPORTED`: TS 3.0 has no per-player bullet trails. Its client draws
  `sprites/bullet_trail.spr` trails (`TS_BulletTrail` 0x10046240) only on the client-side bullets
  it simulates. It does that under slow motion (time scale < 0.99) or when the server's
  `realbullet` is on (`SrvSett` key 0, client `0x1018d16c`), which is a server-wide rule that also
  moves the server to simulated bullets. TS 2.x's per-player flag (tsx's `TSX_BTRAIL_OFFSET`) is
  gone.
* `run_powerup` keeps the held power-up, which TS can't express (there the held one is the one
  that runs). A plugin's run ends without clearing the held power-up and without dropping
  anything on death. A pickup during a plugin's run doesn't clear it. A used power-up that was
  running when a plugin starts one is spent, as if it had run out. Plugin runs ignore TS's
  "You can't use Slow-Motion now!" block (`slowmatch`'s TSGameRules+0x18), and they work on dead
  players and observers too, where TS's expiry ends them on the next PreThink.
* `set_speed`'s aura is checked when `SlowMotionFlow` runs (every 0.5 s, the first time on the
  next PreThink). A player caught once stays slowed until the speed ends, as TS's activator list
  only grows. A speed above 1 makes the activator and the caught players faster (the movement
  frame time is scaled by `fuser1`).
* `set_free_slots` doesn't change the weapons carried. The speed follows the new value as TS's
  `GetSpeedBySlots` computes it (330 for The One or an empty inventory).
* `set_weapon_ammo` on a grenade or blade stack sets the count but not the free weight it takes.
* Private-data mirror (§4.1): a write to `m_iClip` sets the gun in hand's clip, where in TS it only
  changed the copy `GetWeaponData` reads; a write to any other mirrored word that isn't taken
  back is lost at the next sync, where TS kept it (and might have read it). A write is applied up
  to a frame late, and a write the game's own change overtook in that frame wins over it.
* `melee_attack` is called for kung fu only (TS 3.0's and TS 2.1 mode's `CCombatAttack`), not the
  stock hit, the blades or the stunts' jump and backflip kicks. A blocked or changed move isn't
  predicted by the client, which plays its own swing.
* `kill_scored` in TS 2.1 mode carries 2.1's scoring, which has no ` (victims_weapon)` test and no
  `glast_headshot`: those bits are never set there. Its spree tags and `killer_spree` only change
  on a scoring kill in DM/TDM, where 2.1's spree part runs.
* `player_stunt`'s kinds are the port's reading of TS 3.0's moves (`TSState` no longer carries
  them).
* Version 6: `entity_touch` reports bots only, so the touch path stays cheap for everyone else.
  `TELEPORTED` is a distance test (half again the speed's reach, plus 32 units), not a hook on
  every teleport, so a short teleport can pass unseen and a fast push by a mover can be reported.
  `CombatState`'s end times ignore the time scale: a slowed player's timers run out later. A
  `DamageEvent`'s hit is the victim's last `TraceAttack` in the same frame: a refused trace (a
  team mate) followed by a direct damage in that frame would lend its hitgroup to that damage.
  `entity_key` keeps only the keys that aren't entvars fields.
* Version 7: a detached plugin's commands and cvars stay registered with the engine (doing
  nothing, and keeping their values): GoldSrc can't remove either. A backend that registers late
  takes over only from native; while another plugin backend runs, the map reload (`reload.rs`)
  decides as for any `ts_bot_backend` change.
* Entity hooks: §7.6.

## 6. Tests

* Version 7 (`cargo test`): the interface name (`006` refused), the `player_flag` bits distinct
  (`ts-plugin-api`); the registry: ids never reused after a failed load or a detach (#291),
  names unique across loaded and attached plugins, letter case ignored; an attached plugin's name,
  data directory, log prefix, backend (registered before native, which still comes first) and
  listeners, all gone after `detach_plugin`, and a new id for a second attach
  (`plugins::tests`); a listener's owner (`a_detach_drops_the_owners_listeners_only`); when a late
  backend takes over (`bot_backend::tests::a_late_backend_takes_over_native_only`); the snapshot's
  kung-fu and superjump flags (`plugin_api::frame`). `cargo xtask plugins` runs the stub's version
  7 checks (bot-plugins.md §6): `attach_plugin` (the name rules, `EXISTS` for its own loaded name,
  its data directory, a command, a backend and an owned listener), `detach_plugin` (the id dead
  for `data_dir` and `register_bot_backend`, the listener gone, a loaded plugin's id refused, a
  new id after) and `snapshot_powers` (the flags against `player_info`), and `frame_info`'s team
  scores against `game_state`.

* Version 6 (`cargo test`): the interface name (`005` refused), the `stunt_bits` behind
  `STUNTING`; `run_cmd`'s argument checks (flags, `msec` 0), `AUTO_MSEC`'s carry (60 frames of
  1/60 s add up to a second, never 0, a byte), `SET_VIEW`'s choice of view and body angles, the
  teleport distance test (`plugin_api::bots`); the player flags, inventory serials, combat end
  times and item respawn times (`plugin_api::frame`); the damage event's hit (`plugin_api::events`);
  the key table (`plugin_api::keys`); the countdown and 2.1's end-time `ends_at`. `cargo xtask
  plugins` runs the stub's version 6 checks (bot-plugins.md §6): `players` against `player_info`
  and the engine, `frame_info` against `game_state` and the globals, `items` against
  `entity_info`, the inventory serial moving with the inventory, the combat timers after a
  kung-fu move, `run_cmd` with `SET_VIEW | AUTO_MSEC` (the spawn's `FIXANGLE` adopted, the view
  kept, landings, the respawned victim's `TELEPORTED`, the game time put back), `entity_key`, the
  sound's channel and source, touches, the scripted kill's damage event, a fired M4
  (`ts_give`) and a Beretta handed over from a `ts_groundweapon` with `Ham_Use` (`item_pickup`).
* Version 5 (`cargo test`): the interface name (`004` refused); the mirror's offsets against the
  decomp (both layouts, `tsxaddon.inc`'s ints), the words it writes, the writes it takes back
  (reserves and clips with their ranges, `m_iClip`, a weapon not owned, the read-only words
  rewritten) and a new gun entity starting over (`pdata::tests`); the melee chain (each plugin
  sees the last one's values, the first block stops it, bad values ignored) and listener
  handles (`plugins::tests`); `CCombatAttack`'s hook seeing the move's delay and damage, its
  changes reaching the hit and timers, and a block leaving only the delay
  (`ts30-gameplay` `gun::tests::kungfu_melee_attack_hook`, and 2.1's `ts21-gameplay`
  `shared::gun::tests::kungfu_melee_attack_hook`); the scored kill's weapon id, tag bits and name
  (`ts-server` `rules::scoring::kill_report_tests`, 3.0's log tags `ts30-server`
  `scoring::kill_report_tests`), 2.1's spree outcomes and tags and that its `PlayerKilled` sends
  the report (`ts21-server` `scoring::tests`). `cargo xtask plugins` exercises them in a live
  server through the stub (bot-plugins.md §6): a listener added (and a short descriptor refused),
  heard and removed, `private_size`, a bot's dive (`player_stunt`), a used double fire rate
  (`player_powerup`), kung-fu moves changed and blocked (`melee_attack`, each answer checked by
  when the next move comes), and a kill through `Ham_TakeDamage` (`kill_scored`), in TS 3.0 mode.
  The callbacks and the mirror are also `tsmod-amxx-rs`'s e2e (`tsx_e2e.sma`, `ham_e2e.sma`).
* Version 4 (`cargo test`): the interface name, the team scores' order and unnamed teams, every
  new `Ham` number and signature (and `TS_GiveSlowMul`'s deviation), which classes the gun's
  functions and the queries fire for, and the gun's virtual calls reaching the host's hook points
  in `ItemPostFrame`'s order, a skipped one not running (`shared` `weapons::gun::tests`). `cargo
  xtask plugins` adds the stub's `team_scores`, `objectives`, `create_entity` and `gun_hook`
  checks (bot-plugins.md §6).
* Version 3 (`cargo test -p ts-server plugins`, `plugin_api`, `bot_backend`; `-p tsmod-test`;
  `-p xtask`): the naming rule on all four platforms (longest suffix, case on Windows, bad
  names, other files), ELF/PE machine detection, a directory scan (order, other platforms,
  junk, directories), the descriptor checks (refusal, size, version, the blank descriptor), the
  log lines, data directories, command names, `run_cmd`'s `ts_bot_stop` and argument checks,
  `buy`'s checks, rounds, `Args` tokenizing, backend selection, quota names.
* `cargo xtask plugins` (all roots): the stub and the loader's fixtures on a dedicated server
  (bot-plugins.md §6).

* Unit tests (`cargo test -p ts-server plugin_api`, `throwers`): status codes and their order,
  deferral, the snapshot fallback and its user-id check, every argument range, the power-ups that
  run and their normal lengths, the snapshot fields, `set_weapon_ammo` (owned records, reserve
  types, the akimbo split, the HUD flags), `set_speed`'s floor and end, the `TSSlowMo` override,
  the thrower table, and compile-time checks that the private blocks cover TS's objects and keep
  hamsandwich's slots clear.
* `CreateInterface` out of the engine (ctypes on `ts_amd64.so`): `TSRS_PluginApi001` gives
  `IFACE_OK`, size 88 (64-bit), version 1, and all function pointers set (version 2 makes it
  136). Other names and a NULL
  name give NULL and `IFACE_FAILED`.
* AMX Mod X 1.10 on ReHLDS 3.15 with Metamod-R, Linux and Windows (`ref/amxx-testenv`,
  `ham_test.sma` and `pdata_test.sma`): `RegisterHam(Ham_Spawn, "player")` fails cleanly on both
  (previously a crash on Windows). `get_pdata_int(id, 100)` and `get_pdata_int(id, 0)` read 0
  (previously heap garbage and the tag). Both servers keep running with their bots. (Version 4
  results; since version 5 int 0 is `pev` on Linux, §4.)
* Export lists: `nm -D --defined-only` of `ts_i386.so` / `ts_amd64.so` gains only
  `CreateInterface`. `objdump -p` of `mp.dll` / `mp_amd64.dll` is unchanged. Version 2 adds no
  export.
* Entity hooks (`cargo test -p ts-server ham`): the 481 `Ham` names and their numbers, and every
  supported function's argument and return kinds against `hooklist`'s signature (and both lists
  against `ref/amxmodx`'s `ham_const.inc` / `hook_native.cpp` when that is checked out next to
  the repository); marshalling of each kind (NaN and unknown entities ignored, NULL as -1, the
  `TraceResult` and `KeyValueData` pointers); watch gating (armed only with a hook installed);
  pre-hook argument changes reaching the function and the post hook; SUPERCEDE with the post call
  still made and a zero `orig_ret`; the post call's action deciding the return; hooks run inside
  `reach` (the lend); the nesting limit for a hook re-running its own function and for
  `ham_execute` from a hook (`BUSY` at depth 8, every level's function run, the stack unwound);
  `ham_execute`'s statuses and their order (`NOT_PLAYER`, `UNSUPPORTED`, `INVALID` for the count,
  a kind, an entity not in use or a missing pointer, `BUSY` for an object held further up);
  ExecuteHam skipping hooks for its own call only; the class registry (one id per type, link
  order, case-sensitive names). `world.rs`: a slot's type, presence and lendability in every state.
* In engine (FWGS dedicated server, `ts_dojo`, four bots, `sv_cheats 1`, `ts_dev_hamlog 1`): the
  log shows Spawn before and after for every map entity and for each player's first spawn and
  respawn (the inner path), TakeDamage with its inflictor, attacker, damage, bits and return, and
  Killed nested inside the killing TakeDamage. Hooks under the AMX Mod X module are for
  `tsmod-amxx-rs` to test.

## 7. Entity hooks (version 2)

AMX Mod X's hamsandwich hooks C++ virtual functions by patching vtables (`RegisterHam`) and calls
them through the vtable (`ExecuteHam`, `ExecuteHamB`). The port's entities are Rust objects in a
slab (design.md §4.2), so the server calls the plugin's one hook around each function it has, and
runs functions on request. The contract is `ts-plugin-api`'s module docs ("Entity hooks").

### 7.1 Numbering and arguments

Functions are numbered by AMX Mod X 1.10's `Ham` enum (`ham_const.inc`, `Ham_Spawn` = 0 through
`Ham_SC_Player_IsObserver` = 480; `ham::NAMES`), and their arguments and return value follow the
signature `hook_native.cpp`'s `hooklist` gives that number (`ham::SIGS`): Entvar, Cbase and Edict
are `ENTITY` (an index, -1 for NULL), Bool and Short are `INT`, pVector and pFloat are
`VECTOR_OUT` / `FLOAT_OUT`, Trace is `TRACE` (a pointer to a real `TraceResult`), Str is `STRING`.
`Ham_Keyvalue`'s int is the `KeyValueData *` (`p`, and `i` its low 32 bits, as hamsandwich hands
plugins the pointer).

### 7.2 Where the hooks run

Each function's callers go through `ts_server::ham` instead of `world::dispatch`:

* **Outer calls** reach the object through its edict (`ham::spawn`, `think`, `touch`, `use_`,
  `take_damage`, ...). If the function is watched and a hook installed, the pre hook runs while
  the object is still in its slot, then `world::dispatch` takes it out and runs the function, and
  the post hook runs once the object is back. They fire only for an object in its slot: one that
  a dispatch further up the stack holds is skipped by `world::dispatch` as well (no call), so
  there is nothing to hook. `TakeHealth` and `TS_GoSlow` don't take the object out (they change
  entvars, or reach a player lent to its slot through `world::run_now`), so they also fire for a
  lent player.
* **Inner calls** happen inside the object's own dispatch, on the `&mut` its caller holds:
  `Player_Jump` and `Player_Duck` from `PreThink`, `Killed` from `CBasePlayer::TakeDamage`, a
  player's own `TS_GoSlow` (slow-motion flow and release, headshot death), the
  player's respawn `Spawn`, its time-based `TakeHealth`, the HL items' `Respawn`. Each hook call
  lends the object back to its slot and opens it (`world::lend_open`: `lend`, then `run_now`), so
  the hook reaches the entity exactly as for an outer call, and the function goes on with the
  object as the hook left it.
* **Unwatched**, every entry point is the plain call after one test of a bit (`Hooks::armed`, set
  only while a hook is installed and the plugin watches that function).
* The pre hook's argument changes are decoded back into the native arguments (an entity index
  not in use, a NaN or infinite float or vector component, or an unknown use type is ignored,
  with a `[ts] ham error` line for the entity). `ham`, `this`, `class` and `argc` are restored
  after it. The post hook sees the arguments as the function got them.
* The post hook always follows the pre hook, also when the pre hook superseded the function or
  the function could no longer reach its object (removed by the pre hook). `orig_ret` is then
  the zero value. The post call's action alone decides the return value (`OVERRIDE`: `ret`,
  otherwise `orig_ret`); `ret` keeps what the pre hook set, so a module that folds the pre result
  into its post action keeps it.

### 7.3 Re-entrancy

Hooks run with no `RefCell` borrow held and their object in its slot, so a hook may use the
whole API on it, run functions on it or on others (`ham_execute`, which goes through the same
entry points), deal damage, and create or remove entities. A function whose object was removed
by its pre hook doesn't run. `ham_execute` copies the `HamCall` (and a `TraceResult` /
`KeyValueData`) in and writes `ret` back, so passing the hook's own `HamCall` is fine.
`run_hooks = 0` (ExecuteHam) skips the hooks for that call only: a `Killed` that its
`TakeDamage` causes runs them as usual.

Hook calls nest at most `ham::MAX_DEPTH` = 8 deep. Deeper, functions run without hooks and
`ham_execute` refuses with `BUSY`, each with a `[ts] ham error` line, so a plugin that re-runs a
function from its own hook (`ExecuteHamB(Ham_Spawn, id)` in a Spawn hook) stops with an error
instead of overflowing the stack. Calling the plugin's hook is FFI: `ts-server-dll`'s `call_hook`
is the one `unsafe` call, behind the safe `HookFn` the game crate holds.

### 7.4 Classes

`class_id(classname)` gives the class of a classname linked with `link_entities!` (the names
the engine creates through exports), and `entity_class_id` that of a live game entity: one id
per Rust type, numbered in link order from 1 when first asked and stable for the server process
(`ham/classes.rs`). Classnames sharing a type share a class, as hamsandwich's hook on one vtable
covers every name of that C++ class: `func_door` / `func_door_rotating` / `func_water`, the HL
weapon names and `ts_groundweapon`, the four `info_player_team*`. An entity of a type nothing
links gets the next id the first time it is asked about. `world.rs` keeps each object's type in
its slot, so the class is known while the object is out.

### 7.5 The functions

"Fires" is `ham_supported` = 1: the hook runs when the game calls it, and `ham_execute` runs it.
"Execute" is 0: TS has it, the port computes it on request but never calls it as a hookable unit,
so hooks don't fire. Everything else is `UNSUPPORTED`. Addresses are `ts_i386.so`.

| # | `Ham_` | Signature | Fires | Execute | Port |
|---|---|---|---|---|---|
| 0 | `Spawn` | `Void_Void` | all | | `DispatchSpawn`, `ClientPutInServer`, a player's respawn (inner), bots, the game's create helpers (projectiles, `ExplosionCreate`, power-ups, slow-motion points, the CTB case, the sound list), the API's `spawn_entity` |
| 2 | `Keyvalue` | `Void_Int` | all | | `DispatchKeyValue`'s class part, after the entvars parser (SDK order); also the API's `entity_key_value`. A superseding hook's `fHandled` counts. |
| 3 | `ObjectCaps` | `Int_Void` | all | | `levelchange::ts_object_caps` (TS's caps by class, 0 for a name TS doesn't know). Fires in `PlayerUse` (0xd9848: the +use search, so a hook can make an entity usable; its `Use` hooks then fire) and the level change's `ChangeList` / `InTransitionVolume` (0xe87c4, 0xe865c) |
| 4 | `Activate` | `Void_Void` | all | | `ServerActivate` |
| 6 | `Classify` | `Int_Void` | all | | `entities::classify`: TS's value by class (2 `CLASS_PLAYER` for players, 1 `func_guntarget`, 4 `monster_hevsuit_dead`, 99 `xen_tree` / `xen_hull`, else 0). Fires in `TEXTURETYPE_PlaySound` (0xe1330, a bullet's impact sound) |
| 8 | `TraceAttack` | `Void_Entvar_Float_Vector_Trace_Int` | all | | `damage::trace_attack` (bullets, melee, explosions, thrown and falling objects). The trace carries `iHitgroup`, `vecEndPos` and `pHit`; changes to the attacker, damage, direction, bits, hit group and end position are used. |
| 9 | `TakeDamage` | `Int_Entvar_Entvar_Float_Int` | all | | `damage::apply`, `ApplyMultiDamage` (`damage::multi::apply`) |
| 10 | `TakeHealth` | `Int_Float_Int` | all | | `items::take_health` (kits, chargers, healing doors, negative `trigger_hurt`, `game_player_hurt`), a player's time-based healing (inner) |
| 11 | `Killed` | `Void_Entvar_Int` | all | | Players: `rules::on_killed` from `CBasePlayer::TakeDamage` (inner); `iGib` is `DMG_NEVERGIB` / `DMG_ALWAYSGIB` (1 / 2) and back. Others: `CBaseEntity::Killed` (0xabd4c) from `CBaseEntity::TakeDamage` (0xab994, `entities::base_take_damage`; `this` out of reach, §7.6) |
| 12 | `BloodColor` | `Int_Void` | all | | 247 for players, -1 (`DONT_BLEED`) else. Fires for the gibs of a gibbed player (`GibMonster` 0xb14e0's `SpawnHeadGib` / `SpawnRandomGibs`) and the `impulse 102` gib |
| 25 | `OverrideReset` | `Void_Void` | all | | a global entity restored over this one |
| 32 | `IsAlive` | `Int_Void` | all | | `deadflag == DEAD_NO && health > 0` (`rules::is_alive`). Fires in `CheckDeads` (0x79c00), `SelectRandomTheOne` (0x7af28), `TSSelectSpawnSpot` (0x76244), `Observer_Idle` (0x99ca4), the TS triggers (`CTSTrigger`/`CTSDMHill::WaitTouch`), the CTB case and base, the power-up and ground weapon/ammo pickups, and the blades' targets (`KnifeFire`/`KatanaFire`) |
| 33 | `IsBSPModel` | `Int_Void` | all | | `SOLID_BSP` or `MOVETYPE_PUSHSTEP`. Fires in `CheckHull` (0x95e28, the melee hull trace) and `CBeam::BeamDamage` (0xba1b8, `env_beam` decals) |
| 36 | `IsInWorld` | `Int_Void` | all | | inside ±4096, slower than 2000. Fires in `CM61Grenade::WaitThink` (0x7cacc) and `CGib::WaitTillLand` (0xb1dd4), on the entity in its own think |
| 37 | `IsPlayer` | `Int_Void` | all | | the player class. Fires wherever the port tests for a player (`rules::has_player`, which stands for TS's `IsPlayer()` and its classname / `FL_CLIENT` forms: touches, uses, damage, scoring, the rules). Guard: a hook can make a player not count, not the reverse (§7.6) |
| 38 | `IsNetClient` | `Int_Void` | all | | false for `mp.dll`'s `CBot`s (`Player::cbot`) and non-players. Fires in `rules::is_net_client` (votes, the bots' chat, the player list); guarded as `IsPlayer` |
| 41 | `Think` | `Void_Void` | all | | `DispatchThink` |
| 42 | `Touch` | `Void_Cbase` | all | | `DispatchTouch`, `game_zone_player`, the Xen tree |
| 43 | `Use` | `Void_Cbase_Cbase_Int_Float` | all | | `DispatchUse`, every target fired (`fire_targets`), buttons, trains, tanks and their controls, effects |
| 44 | `Blocked` | `Void_Cbase` | all | | `DispatchBlocked` |
| 45 | `Respawn` | `Cbase_Void` | HL items | | `CItem::ItemTouch` (inner); returns the item |
| 48 | `Center` | `Vector_Void` | all | | `(absmin + absmax) / 2`. Fires in `PlayerUse` (the line-of-sight test), `SendObjectiveToHud` (0x7ac80, the `Objective` message and the API's `objectives`) and `CTSBomb::WaitThink` (0x8c860, where the bomb explodes) |
| 49 | `EyePosition` | `Vector_Void` | all | | `origin + view_ofs`. Execute-only in practice: TS's callers (`FVisible`, the intermission camera in `CHalfLifeMultiplay::Think`) are inlined in the port |
| 55 | `Player_Jump` | `Void_Void` | player | | `stunt::jump` from PreThink while +jump is held (inner) |
| 56 | `Player_Duck` | `Void_Void` | player | | `stunt::duck` from PreThink (inner) |
| 57 | `Player_PreThink` | `Void_Void` | player | | `PlayerPreThink` |
| 58 | `Player_PostThink` | `Void_Void` | player | | `PlayerPostThink` |
| 59 | `Player_GetGunPosition` | `Vector_Void` | player | | `origin + view_ofs`. Fires for every shot (`ShootBulletsPlayer`'s source, with the shooter lent) and melee swing (`ColdCock`, `KatanaFire`, `KnifeFire`, `CCombatAttack`) |
| 65 | `Item_CanDeploy` | `Int_Void` | | `weapon_tsgun` | 1 (`CBasePlayerWeapon::CanDeploy` 0xa0170); the port's switches take it as a constant |
| 66 | `Item_Deploy` | `Int_Void` | `weapon_tsgun` | | `CTSGun::Deploy` (0x81030): every weapon switch (`weapon_<id>`, pickups, drops, `GetBetterWeapon`, `SwitchWeapon`, `SelectItem`, `StopControl`) |
| 67 | `Item_CanHolster` | `Int_Void` | | `weapon_tsgun` | 1 (`CBasePlayerItem::CanHolster` 0xf3444); TS 3.0 never calls it |
| 68 | `Item_Holster` | `Void_Int` | `weapon_tsgun` | | `CTSGun::Holster` (0x85674 → `HolsterWeapon`): `m_pActiveItem->Holster()` in `Killed`, `StartObserver`, `RemoveAllItems`, `StartControl`, `SwitchWeapon`, `SelectItem`. The gun's own `HolsterWeapon` calls (a weapon switch, a drop) aren't virtual in TS and don't fire |
| 69 | `Item_UpdateItemInfo` | `Void_Void` | | `weapon_tsgun` | empty (0xf3670) |
| 70 | `Item_PreFrame` | `Void_Void` | `weapon_tsgun` | | `CBasePlayer::ItemPreFrame` (0xdc484) from PreThink once `m_flNextAttack` ran out; the gun's is empty (0xf3450) |
| 71 | `Item_PostFrame` | `Void_Void` | `weapon_tsgun` | | `CBasePlayerWeapon::ItemPostFrame` (0xa03d4) from `CBasePlayer::ItemPostFrame` (PostThink): the attacks, reload and idle below run inside it |
| 72 | `Item_Drop` | `Void_Void` | `weapon_tsgun` | | `CBasePlayerItem::Drop` (0xf1cac) in `RemoveAllItems` (death, observer, disconnect). `ham_execute` on a gun a player still holds does nothing (guard) |
| 77 | `Item_UpdateClientData` | `Int_Cbase` | `weapon_tsgun` | | `CTSGun::UpdateClientData` (0x858c8, `WStatus`, `WeaponInfo`, ...) from `CBasePlayer::UpdateClientData`; returns 1 |
| 78 | `Item_GetWeaponPtr` | `Cbase_Void` | | `weapon_tsgun` | `this`; the port reaches the gun without it |
| 79 | `Item_ItemSlot` | `Int_Void` | | `weapon_tsgun` | 2 (`CTSGun::iItemSlot` 0x87264) |
| 85 | `Weapon_SendWeaponAnim` | `Void_Int_Int_Int` | `weapon_tsgun` | | every view-model animation the gun plays (`DefaultDeploy`, `WeaponIdle`, `SecondaryAttack`, the grenade and reload ones); the arguments the hook leaves are sent |
| 87 | `Weapon_PrimaryAttack` | `Void_Void` | `weapon_tsgun` | | `CTSGun::PrimaryAttack` (0x81a20) from `ItemPostFrame` |
| 88 | `Weapon_SecondaryAttack` | `Void_Void` | `weapon_tsgun` | | `CTSGun::SecondaryAttack` (0x817a8) |
| 89 | `Weapon_Reload` | `Void_Void` | `weapon_tsgun` | | `CTSGun::Reload` (0x84d5c) from `ItemPostFrame` and `WeaponIdle` (the empty-gun and shotgun reloads) |
| 90 | `Weapon_WeaponIdle` | `Void_Void` | `weapon_tsgun` | | `CTSGun::WeaponIdle` (0x854dc) |
| 92 | `Weapon_ShouldWeaponIdle` | `Int_Void` | `weapon_tsgun` | | 1 (0x87274). Asked after an attack or reload, or with an attack button held (0xa07a0); 0 skips that frame's `WeaponIdle`, as in TS |
| 93 | `Weapon_UseDecrement` | `Int_Void` | | `weapon_tsgun` | 1 (0x8726c). The port's weapon timers only count down, the form this answer selects, so an override couldn't be honoured |
| 94 | `TS_BreakableRespawn` | `Int_Int` | `func_breakable`, `func_pushable`, the doors | others (empty, 0xac5ac) | `RespawnEntities` on a round restart (0x77764) |
| 95 | `TS_CanUsedThroughWalls` | `Int_Void` | all | | `entities::ts::can_use_through_walls` (0 for ground weapons, ammo, dropped guns, the case). Fires in `PlayerUse` |
| 123 | `TS_GiveSlowMul` | `Float_Void` (port) | all | | `max(fuser1, 0.01)` (`CBaseEntity::GiveSlowMul` 0xac5b4, the player's 0x92960; the gun's `FixedSlowAmt` form). Fires for the player's weapon timers and spreads (`GunHost::slow_mul`), the stunts (`stunt::slow_mul`), and the CTB case's and dropped guns' landing and bounce. Port deviation: returns the float (§7.6) |
| 124 | `TS_GoSlow` | `Void_Float_Int` | all | | `rules::powerup::go_slow` and `go_slow_self` (slow-motion activators and, when their power-up ends, the players they slowed; the headshot death; `globalslow`, the slow-motion triggers) |
| 125 | `TS_InSlow` | `Int_Void` | all | | `rules::powerup::entity_in_slow`. Fires in the slow-motion triggers and points (`CTSSlowMotion::WaitTouch`, `CTSSlowMotionPoint::WaitThink`) and `GlobalSlowMotion2` (0x79720) |
| 126 | `TS_IsObjective` | `Int_Void` | all | | true for `ts_bomb`, `ts_hack`, `ts_teamescape`, `ts_wingiver` / `ts_scoregiver`. Fires in `InitObjectives` (0x785d8) and `CTSObjectivePointer::Use` (0x8bbd8) |
| 127 | `TS_EnableObjective` | `Void_Int` | all | | the objective's `EnableObjective(on)` (a giver's counter and latch start over); nothing else. Fires where `IsObjective` does |
| 128 | `TS_OnFreeEntPrivateData` | `Void_Void` | all | | nothing (TS's is empty, 0xac650); fires as the engine frees the entity (`OnFreeEntPrivateData` 0xab788) |
| 129 | `TS_ShouldCollide` | `Int_Cbase` | all | | `pfnShouldCollide` |
| 418 | `TS_Weapon_AlternateAttack` | `Void_Void` | `weapon_tsgun` | | `CTSGun::AlternateAttack` (0x81ba8, +attack3: fire mode, laser, ...) from `ItemPostFrame` |

**The gun's functions (`Item_*`, `Weapon_*`).** TS's weapons are one `CTSGun` per player
(`weapon_tsgun`) holding up to six guns; the port keeps its state in the player's inventory
(`weapons::Inventory`), and the `weapon_tsgun` entity only follows the player. Its functions fire
on that entity's class (`class_id("weapon_tsgun")`), `this` the entity, around the calls TS makes
through the gun's vtable. The gun in hand is the context: `entity_info(this)` gives the holder
(`owner`) and its weapon in hand (`weapon`), and `player_info(owner)` the rest. During the hook
the holder is lent to its slot, gun and all, so the whole API works on it, except in
`SendWeaponAnim` (the holder is inside its weapon frame, out of reach; the gun entity isn't).
`ham_execute` runs the function on the holder's gun (`BUSY` for a `weapon_tsgun` no player holds,
where TS would run it without an `m_pPlayer`, or whose holder is out of reach).

**Queries** (`ObjectCaps`, `Classify`, `IsPlayer`, `Center`, `GiveSlowMul`, ...) fire where the
game asks them, listed above. They don't take the object out of its slot, so the hook reaches
`this` when it is in its slot or lent; when the game asks about an entity inside that entity's own
call (a grenade's think asking its own `IsInWorld`, a player's weapon frame its own
`GiveSlowMul`), `this` is out of reach for the hook (its entvars are readable; the API answers as
for a player inside its own callback).

The rest of TS's gamedata (`virtual.games/ts/offsets-common.txt`) is `UNSUPPORTED`:

* `Precache`, `SetObjectCollisionBox`: the port runs them inside the class's spawn or restore
  and the engine's own bounds code, never as a call of their own.
* The monster, AI and helper virtuals (`DeathNotice`, `TraceBleed`, `IsTriggered`,
  `MyMonsterPointer`, `GetToggleState`, `AddPoints`, `AddPlayerItem`, `GiveAmmo`, `GetDelay`,
  `IsMoving`, `DamageDecal`, `TeamId`, `GetNextTarget`, `FVisible`, `BodyTarget`, ...): the port
  has no function for them, or inlines TS's call where it ports it.
* `Player_ShouldFadeOnDeath`, `Player_ImpulseCommands`, `Player_UpdateClientData`: plain code
  inside PreThink, PostThink and `UpdateClientData`, not yet split out as hookable units.
* `Item_AddToPlayer`, `Item_AddDuplicate`, `Item_AttachToPlayer`, `Item_Kill`,
  `Weapon_ExtractAmmo`, `Weapon_ExtractClipAmmo`, `Weapon_AddWeapon`: TS calls them only while a
  `weapon_tsgun` joins a player (`AddPlayerItem` 0xdc0c4, `DefaultTouch` 0xf1b40, the duplicate
  path), which the port folds into `weapons::add_gun` without the entity-level calls.
* `Item_PrimaryAmmoIndex`, `Item_SecondaryAmmoIndex`, `Weapon_PlayEmptySound`,
  `Weapon_ResetEmptySound`, `Weapon_IsUsable`, `Weapon_RetireWeapon`: **no call site in TS 3.0**.
  `ts_i386.so` makes no virtual call of the last four slots on the gun, and the ammo indices are
  asked only by `PackDeadPlayerItems` (0xd8854), which nothing calls.
* `Item_GetItemInfo`: hamsandwich's TS gamedata has no offset for it (the slot is
  `CTSGun::GetItemInfo` 0x85dd0).
* The `cycler_weapon`'s item functions: only `weapon_tsgun` is mapped.
* `TS_RespawnWait` is `Deprecated` in hamsandwich itself.

### 7.6 Deviations and limits

* The hooks and `ham_execute` are a port addition with hamsandwich's semantics; TS has no plugin
  interface.
* A class is a Rust type, not a C++ class: classnames TS builds from different classes but the
  port from one type share hooks (`info_player_team1..4` with `info_player_deathmatch`), and the
  port's generic types are one class per parameter (`item_healthkit` and `item_battery` differ, as
  in TS).
* Calls the port makes without the hook: a breakable's or door's `Spawn` inside its own
  `BreakableRespawn`, `CTestEffect`'s restore `Spawn`, and `Use` calls the game makes on an object
  it already holds. In TS each would be a virtual call hamsandwich sees. Query call sites the port
  inlines (listed per function above) don't fire either.
* Outer hooks don't fire for an object a dispatch further up the stack holds (a `Touch` of a
  player during its own think); the port doesn't run the function then either.
* `Killed` of a non-player fires inside its `TakeDamage`, which holds it: `this` is out of reach
  for the hook (entvars readable, `ham_execute` on it `BUSY`). A superseding hook keeps it alive at
  0 health, as in TS.
* `IsPlayer` / `IsNetClient` guard: a hook's 0 is honoured, a 1 for an entity that isn't a player
  is not (the port has no player object for it; TS would treat its memory as a `CBasePlayer`).
* `TakeHealth`'s bits are always `DMG_GENERIC`, as in every TS caller; changes are ignored.
  `Keyvalue`'s strings are read-only.
* `ham_execute(Ham_TraceAttack)` only adds to the multi-damage, as TS's `TraceAttack` does;
  nothing applies it unless the plugin deals the damage. Of the trace, the port uses `iHitgroup`
  and `vecEndPos`; `flFraction` is 0 and the plane fields are empty.
* `ham_execute(Ham_Killed)` on a non-player runs `CBaseEntity::Killed`, which removes it, also
  for classes whose TS `Killed` does more (the dead monsters).
* `Ham_TS_GiveSlowMul` returns its float (kind `FLOAT`, `Float_Void`), where hamsandwich declares
  `Void_Void`; `tsmod-amxx-rs` maps it so `GetHamReturnFloat` / `SetHamReturnFloat` work.
* `ts_dev_hamlog 1|0` (sv_cheats) is a dev aid that installs the server's own hook and logs
  Spawn, TakeDamage and Killed (`[ts ham]` lines). It is refused while a plugin's hook is
  installed, and a plugin's `ham_set_hook` replaces it.

## 8. Version 2 → 3

Version 3 (issue #104) is a break: `CreateInterface` answers only `TSRS_PluginApi003`, and the
table is laid out anew (host section first). Every version-2 function is still there, with the
same arguments and results:

| Version 2 | Version 3 |
|---|---|
| `TSRS_PluginApi001`, `version` 1/2, "check `size` for fields after version 1" | `TSRS_PluginApi003`, `version` 3; check `version == 3` and `size` |
| `player_info` (`PlayerInfo` 18 fields) | `player_info`; `PlayerInfo` gains `alive`, `is_bot`, `team`, `has_flag` (after `joined`), the rest unchanged |
| `set_cash`, `set_free_slots`, `set_award`, `run_powerup`, `set_speed`, `set_physics_speed`, `set_bullet_trail`, `set_weapon_ammo` | unchanged, in the players section |
| `projectile_owner` | unchanged, in the entities section |
| `ham_set_hook`, `ham_watch`, `ham_supported`, `class_id`, `entity_class_id`, `ham_execute`, `HamCall`, `HamArg` | unchanged, last in the table |
| `BUSY` "ham_execute only" | also `run_cmd` |
| — | new: `engine_funcs`, `globals`, `log`, `data_dir`, `map_name`, `register_command`, `register_cvar`, `register_bot_backend`, `player_inventory`, `entity_info`, `master_triggered`, `game_state`, `create_bot`, `kick_bot`, `run_cmd`, `bot_command`, `select_weapon`, `buy`, `buy_item`, `clear_loadout`, `drop_weapon`; codes `FULL`, `EXISTS`; `TSRS_PluginLoad` and the `Plugin` / `BotBackend` / `LoadInfo` descriptors |

## 9. Version 3 → 4

Version 4 (issues #117 and #99) is a break: `CreateInterface` answers only `TSRS_PluginApi004`
and `Plugin::api_version` must be 4. Every version-3 entry is still there, with the same
arguments and results, except where the table says otherwise:

| Version 3 | Version 4 |
|---|---|
| `TSRS_PluginApi003`, `version` 3 | `TSRS_PluginApi004`, `version` 4 |
| `GameState` | gains `team_scores[MAX_TEAMS]` (4) at the end |
| `entity_info` of a held `weapon_tsgun`: `weapon` 0 | the holder's weapon in hand |
| `Ham_ObjectCaps`, `Classify`, `BloodColor`, `IsAlive`, `IsBSPModel`, `IsInWorld`, `IsPlayer`, `IsNetClient`, `Center`, `EyePosition`, `Player_GetGunPosition`, `TS_CanUsedThroughWalls`, `TS_GiveSlowMul`, `TS_InSlow`, `TS_IsObjective`, `TS_EnableObjective`, `TS_OnFreeEntPrivateData`: `ham_supported` 0 | 1: they fire where the game asks them (§7.5) |
| `Ham_Killed` on non-players: 0 | 1 (`CBaseEntity::TakeDamage`) |
| `Ham_TS_GiveSlowMul`: `Void_Void`, nothing returned | `Float_Void`: `max(fuser1, 0.01)` |
| `Item_*` / `Weapon_*`, `TS_Weapon_AlternateAttack`: `UNSUPPORTED` | on `weapon_tsgun` (§7.5) |
| — | new: `objectives` and `ObjectiveInfo` (`OBJECTIVE_NAME_LEN`), `create_entity`, `entity_key_value`, `spawn_entity` (after `game_state`), `MAX_TEAMS` |

## 10. Version 4 → 5

Version 5 (tsmod-amxx-rs#8 and #10, #197) is a break: `CreateInterface` answers only
`TSRS_PluginApi005` and `Plugin::api_version` must be 5. Every version-4 entry is still there,
with the same arguments and results:

| Version 4 | Version 5 |
|---|---|
| `TSRS_PluginApi004`, `version` 4 | `TSRS_PluginApi005`, `version` 5 |
| `Plugin` ends with `ammo_changed` | gains `player_stunt`, `player_powerup`, `melee_attack`, `kill_scored` (§1.3) at the end; `KillInfo`, `WEAPON_NAME_LEN`, the `kill` bits |
| — | new, last in the table: `add_listener`, `remove_listener` (§1.3), `private_size` (§4); the `stunt` kinds |
| private data: the tag, zeroes elsewhere | `pev` and the mirror (§4.1) |

## 11. Version 5 → 6

Version 6 is a break for bot backends (what tsmod-rcbot-rs worked around: a view stuck after a
spawn, its own `msec` estimate, per-player `player_info` calls and entity scans every frame,
classname guesses for items, no hit location, no `master` keys). `CreateInterface` answers only
`TSRS_PluginApi006` and `Plugin::api_version` must be 6. Every version-5 entry not named here is
unchanged:

| Version 5 | Version 6 |
|---|---|
| `TSRS_PluginApi005`, `version` 5 | `TSRS_PluginApi006`, `version` 6 |
| `run_cmd(player, cmd)`; `BotCmd` ends with `msec` (1..255) | `run_cmd(player, cmd, out)`: `BotCmd` gains `flags` (`cmd::SET_VIEW`, `cmd::AUTO_MSEC`; `msec` 0 = auto), `out` a `BotCmdResult` (`cmd_event` bits); the globals' time is put back |
| `Plugin::player_damaged(victim, attacker, inflictor, damage, bits, health)` | `Plugin::damage(&DamageEvent)` in its place (adds armour, hitgroup, weapon, direction, position) |
| `Plugin::sound(entity, origin, sample, volume, attenuation)` | adds `channel` and `source_player` |
| `game_state`'s `ctb`: searched on every call | found once per map at `ServerActivate` |
| — | new, last in `Plugin`: `entity_touch`, `weapon_fired`, `item_pickup`; last in the table: `players` (`PlayerSnapshot`, `CombatState`, `player_flag`, `stunt_bits`), `items` (`ItemInfo`), `frame_info` (`FrameInfo`), `entity_key` |

## 12. Version 6 → 7

Version 7 (#290) is a break for metamod plugins: `CreateInterface` answers only
`TSRS_PluginApi007` and `Plugin::api_version` must be 7. Every version-6 entry not named here is
unchanged:

| Version 6 | Version 7 |
|---|---|
| `TSRS_PluginApi006`, `version` 6 | `TSRS_PluginApi007`, `version` 7 |
| `add_listener(listener)` | `add_listener(plugin, listener)`: the owner, `NO_PLUGIN` or an attached plugin (§1.3) |
| a `CreateInterface` caller has no id | new, last in the table: `attach_plugin(name) -> PluginId`, `detach_plugin(plugin) -> c_int` (§1.4) |
| `register_bot_backend`, `data_dir`: loaded plugins only | attached plugins too; a backend registered after the map's selection takes over from native when `ts_bot_backend` names it |
| `player_flag` ends with `STUNTING` (0x400) | adds `KUNG_FU` (0x800) and `SUPERJUMP` (0x1000) |
| `FrameInfo` ends with `round` | adds `team_scores[4]` |
| plugin ids: the survivors' highest + 1 | a process-wide counter (#291) |

## 13. Version 7 → 8

Version 8 (#294, #304, #321) is a break in name and in the descriptor's size: `CreateInterface`
answers only `TSRS_PluginApi008`, `Plugin::api_version` must be 8, and `Plugin` gains two
callbacks at its end (`client_plugin_state`, `client_plugin_list`, §1.3), so a version-7
descriptor's `size` is refused. Every version-7 entry is unchanged; four functions are new, last in
the table (`crates/server/src/plugin_api/ext.rs`, `client_plugins.rs`):

| Function | Behaviour |
|---|---|
| `ext_message(player, channel, data, len, flags)` | One `TSExt` message (the port's user message for client extensions, registered after TS's 64; client-ext-api.md §10): `string channel` then `len` bytes. To `player` (`MSG_ONE`), or with `player` 0 to every port client, each `MSG_ONE`; `ext_flag::UNRELIABLE` (1) sends `MSG_ONE_UNRELIABLE`. Only clients that hook `TSExt` get it, read at the send as `ClientConnect` reads it (`gameplay::with_connecting_client`): a `_tsrs` version at or above `port::TSPKG_SINCE`, `TSPkg`'s gate with its `ts_dev_tspkg` dev aid (TS 3.0's client and port clients up to 3.0.8 don't hook it; client-plugins.md §12). Checks in this order: `NOT_PLAYER` (no connected player there), `INVALID` (a channel that isn't 1 to 15 bytes of ASCII letters, digits, `_`, `-`, `.`; a payload over `EXT_MESSAGE_MAX` (192) minus the channel and its NUL; NULL `data` with a length; an unknown flag), `UNSUPPORTED` (that player's client doesn't hook `TSExt`: TS 3.0's client, a port client before 3.0.9, a bot, HLTV). Returns the number of clients it went to (0 for a broadcast with no port client). One message is one delivery: the port doesn't chunk (a channel's own protocol does, as the webview's). |
| `client_port_version(player, buf, len)` | The player's `_tsrs` as `major.minor.patch` (`port::parse_userinfo_version`: the string, or the numeric form a `+connect` sends first, #303), its length returned. 0 and "" for a client without the port (TS 3.0's, a bot, HLTV); `UNSUPPORTED` for a port client whose value isn't a version (the `1` of clients from before #303); `NOT_PLAYER` otherwise. For AMX Mod X plugins with a version policy of their own (client-plugins.md §13.1). |

| `offer_client_plugin(player, id)` | One `TSPkg` offer of `id`, a `plugin_offers.toml` entry that passed this map's checks, as `sv_plugin_offer` sends it (recorded for the client, so its answers count, client-plugins.md §6.1): to `player`, or with `player` 0 to every client that hooks `TSPkg`. Checks in this order: `NOT_PLAYER`, `INVALID` (NULL `id`, an `id` that isn't UTF-8, or no checked offer of it: `sv_plugin_offers` lists them), `UNSUPPORTED` (the player's client doesn't hook `TSPkg`: TS 3.0's, a port client before 3.0.9, a bot, HLTV). Returns the number of clients it went to; 0 also when the offer can't be sent now (a relative `url` while `sv_downloadurl` is empty or not `http(s)://`; logged once a map). |
| `client_plugin_version(player, id, buf, len)` | The version of `id` (a plugin id or `manual:<name>`) the player's client has loaded as far as the server knows: its answer `loaded`, else its `tspkg list`, else its answer `have` (the same manifest approved already, loaded at once) (`plugin_offers::clients::Client::loaded_version`). Written like `client_port_version`, its length returned; 0 and "" when the server doesn't know it loaded (also before the list came). `NOT_PLAYER`, `INVALID` (NULL `id`), `UNSUPPORTED` (as above: such a client runs no client plugins the server can learn of). |

Decisions (#321): the spec's `client_plugin_state(edict, id, state)` also carries the answer's
version and reason (both in the `tspkg` line, and a plugin needs the version to tell an update's
`restart` from the old one's), and the list event fires on the list's first closing line only
(a client may send a bare `tspkg list` again; the enforcement keeps counting what came).
`offer_client_plugin` with player 0 offers to every client that hooks `TSPkg`, as
`sv_plugin_offer all` and `ext_message` do. These were first planned for a version 9; version 8
wasn't released yet (v3.0.8 shipped version 7), so they joined it.

Downstream: the AMX Mod X modules (tsmod-amxx-rs#22: the bump, natives for the four functions and
forwards for the two events, and a
metamod guard on raw `message_begin(TSExt)` to clients without `_tsrs`, which the game library
can't refuse) and the bot plugins pinned by port tag (tsmod-rcbot-rs, tsmod-amxx-rcbot: the bump
only) move with their next pin. `cargo xtask plugins` checks both functions on a bot-only server
(`ext_message`, `client_port_version`, `offer_client_plugin` and `client_plugin_version` on a
bot and with bad arguments); `cargo xtask tspkg` part 1 installs the stub on the dedicated server,
where the port client's `tsmod_test_offer` offers through `offer_client_plugin` and the stub logs
both events with `client_plugin_version`'s answer; `cargo xtask client-plugins` sends `TSExt` to the port
client and reads its version back (client-ext-api.md §9).
