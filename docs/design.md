# Design: The Specialists 3.0 in Rust

This repository ports **The Specialists 3.0** (TS) to Rust: the server game library
(`ts_i386.so`/`mp.dll`) and the client library (`client.dll`). The source of truth for behaviour
is the decompilation in [the-specialists-decomp](https://github.com/the-specialists-mod/the-specialists-decomp)
(referred to below as **the decomp**). Its `docs/` describe every subsystem with addresses,
constants, and payloads; this document says how those pieces are organised in Rust.

The codebase started from the minimal `mymod` workspace
([rust-hlmod](https://github.com/CrunchyMonkies/rust-hlmod)), whose general GoldSrc-in-Rust
design (`docs/rust.md` in the parent workspace) still applies.

## 1. Goals

| Goal | How |
|---|---|
| Behave like TS 3.0 | Port logic and constants from the decomp, including the TS changes to player movement. Deviations are listed in §10. |
| **Wire-compatible with TS 3.0** | Same user messages (names, sizes, registration order, payloads), same events, same `delta.lst`, same `clientdata`/`weapon_data` field usage (§2). An original TS client can join this server; this client can join an original TS server. |
| Linux and Windows | Four targets: `i686-unknown-linux-gnu`, `i686-pc-windows-gnu`, `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-gnu` (§8). |
| Engines | Xash3D FWGS (development, native 64-bit), ReHLDS on pre-anniversary HLDS, and stock Valve HLDS post-anniversary (HL25). Steam GoldSrc clients, post-anniversary (HL25); the pre-anniversary client is out of scope. |
| Validated against real content | `cargo xtask` installs into each engine root with the local TS 3.0 content and runs dedicated servers with bots (§9). |

## 2. Wire compatibility contract

The protocol between the two libraries is exactly TS 3.0's, so either side can be swapped for
the original.

* **User messages:** `shared::protocol::MESSAGES` lists all 64 in TS registration order with TS
  sizes. The server registers them in that order in `GameInit` (`messages::register_all`). Payload
  layouts follow decomp `server/architecture.md` §3.2 and `client/architecture-hud.md` §3. Do not
  rename, reorder or resize them. The port's own messages are in the same table after them
  (`protocol::TS_MESSAGES`), registered in this order: `TSExt` (client extension channels,
  `shared::protocol::ext`, `implementation/client-ext-api.md` §10), then `TSPkg` (client plugin
  offers, `shared::protocol::tspkg`, `implementation/client-plugins.md` §6.2). Both sides hook and
  send by name, so the order only has to keep TS's 64 first. `TSPkg` goes only to port clients
  that hook it (`_tsrs` at or above `port::TSPKG_SINCE`; §12 there).
* **Events:** the same `events/*.sc` names and the same `fparam`/`iparam`/`bparam` meanings
  (decomp `server/weapons.md` §6.1, `client/rendering-effects-prediction.md` §4.2).
* **Networked fields** (`delta.lst` is TS's original file, byte for byte; both engines accept its two missing commas):
  * `pev->iuser4` / `clientdata.iuser4`: stunt bitfield (`shared::ts::stunt`).
  * `pev->fuser1` / `clientdata.fuser1` / `weapon_data.fuser1`: per-entity time scale.
  * `clientdata.vuser1..4` and `ammo_*`: TS weapon state (decomp `server/architecture.md` §7.2).
  * `weapon_data_t`: a single entry for the TS gun (decomp `server/architecture.md` §7.3).
  * The `gamedll` conditional encoders `delta.lst` names (`Entity_Encode`, `Player_Encode`,
    `Custom_Encode`) are registered as TS's `RegisterEncoders` does (`ts_server::delta`).
* **Client commands:** TS's own (`tkw`/`tki`/`tka` loadout lists, `weapon_<n>`, `drop_*`, …; decomp
  `server/architecture.md` §4).
* **Strings are bytes.** TS copies chat, player names, team names, map text, `hostname` and the
  MOTD into its messages and log lines byte for byte, whatever their encoding (a Windows client's CP1252 `é` is
  the single byte 0xE9). The port does the same: `hud_wire::Wire::string` writes and reads `&[u8]`,
  payload fields that carry such text are `Vec<u8>` (`bytes` in the `payload!` macro), and the
  server builds those lines with `shared::bformat!` (`shared::text`) instead of `format!`, so no
  byte goes through a lossy UTF-8 conversion (#81). Team and model names are `Vec<u8>` in the
  rules state and in `TeamInfo`/`TeamScore`/`TeamNames`, compared like TS's `strcasecmp` (A-Z
  folded only, `rules::state::team_eq`; #188). The client keeps network strings as one `char`
  per byte (`shared::text::latin1`) and hands the bytes back to the engine's draw and print calls,
  as TS's client passes them to `pfnDrawConsoleString`.

## 3. Repository layout

```
crates/
  goldsrc-sys/   bindgen output from the FWGS headers (per target)
  goldsrc/       safe engine wrappers: server (traces, sounds, events, MsgWriter, cvars, bots),
                 client, edicts (edict.rs), player-move callbacks (pmove.rs), native mouse (platform.rs)
  gameplay/      ts-gameplay: gameplay-plugin ids, channels and registry (implementation/gameplay-plugins.md)
  shared/        runs on both sides: pm (movement), weapons (tables + predicted logic), protocol, ts,
                 gameplay (the PmRules/GunRules hooks), bsp (entity lumps)
  ts30/          the TS 3.0 gameplay plugin: ts30-gameplay (shared hooks), server/ ts30-server,
                 client/ ts30-client; linked only by the two libraries (issue #200)
    data/        weapons.toml, ammo.toml, combat.toml, assets.toml (decoded from the decomp)
  ts21/          the TS 2.1 gameplay plugin: ts21-gameplay (shared hooks), server/ ts21-server,
                 client/ ts21-client; linked only by the two libraries (implementation/ts21.md)
  server/        ts-server: server game logic (rlib, no unsafe)
  server-dll/    ts-server-dll: cdylib -> dlls/ts_i386.so, ts_amd64.so, mp.dll (exports, callbacks)
  plugin-api/    ts-plugin-api: the C ABI table server plugins get from CreateInterface (AMX Mod X
                 modules) or from the server's `tsmod_*` loader (bot backends; issue #104)
  tsmod-test/    the stub `tsmod_test` plugin (a test fixture: loader checks, a trivial bot backend)
  plugin-pkg/    client plugin packages (.tspkg), manifests, signatures, approved.toml and per-file
                 verification; no unsafe, no engine dependency, shared with tsmod-launcher
                 (implementation/client-plugins.md §10)
  client-ext-api/ ts-client-ext-api: the C ABI the client offers client extensions, cl_dlls/tsmod_*
                 libraries it loads once the approval gate allows (implementation/client-ext-api.md)
  tsmod-client-test/ the stub client extension `tsmod_test` and its helper process (a test
                 fixture for `cargo xtask client-plugins`)
  client/        ts-client: client game logic (rlib, no unsafe); `plugins` verifies the client
                 plugins at start, asks about new or changed ones and is the extension loader's
                 approval gate (implementation/client-plugins.md §5, §7.1); `plugins::offers`
                 handles a server's offers: in-game cards, downloads on worker threads (ureq,
                 rustls), install and late loading (§6.3, §7.2); `plugins::menu` is the
                 `cl_plugins_menu` panel (revoke, unblock, delete, every file's hash; §8)
  client-dll/    ts-client-dll: cdylib -> cl_dlls/client.so, client_amd64.so, client.dll; also loads
                 the client extensions (ext.rs; the logic is ts-client's `ext`)
xtask/           build, dist, install, server tests
game/ts/         liblist.gam, gameinfo.txt, delta.lst, game.tga, ts_logo_big.tga (installed over the TS content)
docs/            design.md (this), implementation/*.md (per subsystem), validation.md
```

## 4. Server architecture

### 4.1 Entry points and dispatch

`server-dll/src/lib.rs` exports `GiveFnptrsToDll`, `GetEntityAPI`, `GetEntityAPI2`,
`GetNewDLLFunctions`, and fills every `DLL_FUNCTIONS` slot. Each callback converts the engine's
raw pointers and is routed to a module of `ts-server` (§4.8 explains the split):

| Callback | Goes to |
|---|---|
| `GameInit` | `messages::register_all`, then `rules`, `weapons`, `stunt`, `bot_backend` `game_init` (cvars, server commands, the native bot backend), then the `tsmod_*` plugins load (`server-dll/src/plugins.rs`, `implementation/plugin-api.md` §1.2) and get their `game_init` |
| `Spawn`/`Think`/`Touch`/`Use`/`Blocked`/`KeyValue` | the edict's `Entity` (world dispatch), through `ham` (the plugins' entity hooks, `implementation/plugin-api.md` §7). Keyvalues go to `entities::entvars_key_value` first and only the rest reach the class, as TS `DispatchKeyValue` does. |
| worldspawn spawn | `CWorld::Precache` order: force `sv_gravity`/`sv_stepsize`, `rules::install` (installs the rules, the multiplayer voice icon, queues the server config), soundent and body queue, sentences and texture types, then `precache::{w_precache, client_precache, ts_precache, world_sounds_and_gibs}` (TS's map-wide precaches in TS's order, so model/sound/event indices match), light styles, `sv_zmax`, chapter title, `v_dark`, `mp_defaultteam` |
| `ClientConnect`/`Disconnect`/`Kill`/`UserInfoChanged` | `rules` |
| `ClientPutInServer` | creates the `Player` entity and spawns it; marks op 5 and the client plugin offers due (`plugin_offers`, sent at the first `PostThink`: FWGS drops user messages to clients not yet spawned, #317) |
| `ClientCommand` | `commands::client_command`: the `tsmod_*` plugins first (one may swallow it), then the port's `tspkg` replies (`plugin_offers`), then weapons, stunt, rules, the active bot backend, in that order |
| `PlayerPreThink`/`PostThink` | `Player::pre_think`/`post_think` (hook order in §4.3), through `ham`; after `PostThink`, `plugin_offers::client_ready` |
| `StartFrame` | `bot_backend::start_frame` (the quota, then the active backend's bots: for native, each bot's brain and its `pfnRunPlayerMove`, before the rules as in `mp.dll`), the `tsmod_*` plugins' `start_frame` (a plugin backend runs its bots here), `rules::start_frame` (which runs `plugin_offers::start_frame`: the op 5 kicks and the client plugin rules of `plugin_offers::enforce`, #302), then `plugin_api::start_frame` (the plugin API's player snapshots, and the plugins' weapon/ammo callbacks) |
| `PM_Move`/`PM_Init`/`PM_FindTextureType` | `shared::pm` |
| `UpdateClientData`/`AddToFullPack`/`CreateBaseline` | `net` (+ `Player::fill_client_data`) |
| `GetWeaponData`/`CmdStart` | `weapons` |
| `Save`/`Restore`/`SaveGlobalState`/`RestoreGlobalState`/`ResetGlobalState` | `save` (§4.9) |
| `SaveWriteFields`/`SaveReadFields` | `goldsrc::ext::saverestore` (the engine's own structs, through `raw`) |
| `ParmsChangeLevel` | `entities::stock::levelchange::parms_change_level` (the transition list) |

`server-dll/src/exports.rs` holds the rest of TS's non-entity exports, so the plain-name export
lists match `ts_i386.so` and `mp.dll` (`implementation/stock-entities.md` §7.5):

* `Server_GetBlendingInterface` (both builds), routed to `ts_server::studio`. TS's version
  (0xa3f80) fills in its `svBlending` (whose `SV_StudioSetupBones` only logs "SV_StudioSetupBones
  called.") and stores the engine's studio API and bone matrices, but returns 0, so every engine
  keeps its own server bone setup. There's no server-side bone maths in TS to port; hitboxes and
  studio traces use the engine's.
* `CreateInterface`: mp.dll's tier1 factory has nothing registered, so TS reports `IFACE_FAILED`
  and returns NULL. The port's answers one name, `TSRS_PluginApi007` (version 7), with the plugin
  API table (`ts-plugin-api`, `ts_server::plugin_api`; `implementation/plugin-api.md`), and is
  exported on every platform: on Linux it is the one export `ts_i386.so` doesn't have (port
  addition). The table carries entity hooks (`ts_server::ham`, below), since version 4 also on
  the gun's `Item_*` / `Weapon_*` functions and the query virtuals the game asks.
* `anglemod`, `Length`, `Q_log2` (Linux only): GCC exported these `pm_math.c` helpers. They call
  `shared::pm::{anglemod, length, q_log2}`, in `ts_i386.so`'s x87 arithmetic.

### 4.2 Entity framework (`world.rs`, `entities/`)

* One Rust object per game-owned edict, kept in a generational slab. The edict's private data
  holds the handle, at offset 8 of a zeroed block of at least 0x80 bytes (TS-sized for players,
  `weapon_tsgun` and thrown knives), so AMX Mod X's fakemeta and hamsandwich, which take the block
  for TS's C++ object, neither overflow it nor follow a vtable pointer
  (`implementation/plugin-api.md` §4). Since plugin API version 5 the block also carries `pev`
  and a mirror of the TS members plugins read (`m_pActiveItem`, the gun's ammo and records;
  `pdata.rs`, plugin-api.md §4.1). `OnFreeEntPrivateData` runs the plugins' `Ham_TS_OnFreeEntPrivateData` hook and drops the object.
* **Take-out dispatch:** during a callback the object is moved out of its slot. Engine calls that
  re-enter the game (touch, remove, create, damage to *another* entity) therefore work. Damage to
  *yourself* can't be dispatched; handle it with the `&mut self` you hold. Work for a taken-out
  object can wait for it (`world::defer`); a callback can lend its object back to the slot
  around a call that must reach it (`world::lend`: `world::run_now_or_defer` and `world::run_now`
  then run at once, and `world::with_lent` reads it, e.g. `PlayerKilled` naming the thrown knife
  inside its touch). A player's weapon frame lends the player, inventory and all, around each shot
  and swing (`weapons::host::ServerHost::reach_player`), so what the hit sets off can change it.
* `Entity` trait methods: `key_value`, `spawn`, `think`, `touch`, `use_`, `blocked`,
  `trace_attack`, `take_damage`, `should_collide`, and for save/restore `save_fields`, `restored`
  (TS's `Precache`, which `DispatchRestore` runs) and `override_reset`. `trace_attack` and
  `take_damage` default to `CBaseEntity`'s (`entities::base_take_damage`), `save_fields` to the
  `CBaseEntity` part of the chain (§4.9); the rest are no-ops.
* `entities::link_entities!` exports one C symbol per classname (through
  `goldsrc::export_entity_class!`, so the export lands in the server library even though it's
  declared in `ts-server`). `fire_targets`/`use_targets`
  implement SDK `FireTargets`; `entities::remove` is the deferred `UTIL_Remove` (`FL_KILLME`) and
  `stock::base::sub_remove` the immediate `SUB_Remove`.
* Stock Half-Life classes live in `entities/stock/`, TS classes in `entities/ts/`.

### 4.3 The player

`player::Player` is the `player` entity. Each subsystem keeps its state in its own struct:

| Field | Type | Owner |
|---|---|---|
| `stunt` | `stunt::StuntState` | movement package |
| `inv` | `weapons::Inventory` | weapons package |
| `ts` | `rules::PlayerRules` | rules package |
| `hud` | `player::hud::HudCache` | rules package |
| `tbd` | `player::water::TimeBasedDamage` | rules package (damage types taken, time-based damage, drown recovery) |
| `suit` | `player::suit::SuitQueue` | rules package (the HEV suit's sentence queue, single player only) |
| `bot`, `cbot`, `is_bot`, `next_node_learn` | `Option<bot_backend::BotState>` (TS's `CBot` part: the bot backend's state, the native backend's `bots::Brain`), the class flag, `m_bIsBot`, the node-learning timer | rules package (bots) |
| `drive` | `entities::stock::drive::DriveState` | stock entities (train/tank driving) |

Fields several subsystems read live on `Player` itself: `slow_target`, `active_powerup`,
`powerup_until`, `kungfu`, `superjump`, `slots_used`, `cash`, `team`, `frags`, `deaths`,
`respawn_at`, `joined`, `player_speed` (`m_flPlayerSpeed`, the bots' walking speed). The current time scale is `pev->fuser1` and the stunt state is
`pev->iuser4`, because both are networked.

Hook order, fixed in `player/mod.rs`:

* **spawn:** base player setup, then `rules::on_spawn` (spawn point, loadout), `weapons::on_spawn`, `stunt::on_spawn`, `drive::on_spawn`, `hud::reset`.
* **pre_think:** as in TS `PreThink` (0x908cc), `rules::player_think` first (button edges, `PlayerThink`); during intermission PreThink stops there. Otherwise `drive::pre_think` (train speed control; on the frame the train is lost TS returns from PreThink, so the later hooks skip their TS-tail parts), then `rules` (which runs `hud::update_client_data`, TS's `UpdateClientData`, right after `WaterMove`), `stunt`, `weapons`.
* **post_think:** as in TS `PostThink` (0x91610), during intermission or while dead only the weapon timers run down (plus `rules`); otherwise `drive::post_think` (tank firing / letting go), `weapons` (item post frame, skipped while a tank is in use; once the gun is ready it first runs `player::player_use::impulse_commands`, TS's `ImpulseCommands`: the single `PlayerUse` for every usable entity (stock, TS items, dropped guns), then the spray (201), 99, 204 and the cheat impulses), `stunt`, `rules`.
* **think:** `CBaseEntity::Think`, which the engine calls from the player's usercmd after PreThink once `nextthink` is due: `rules::lifecycle::death_think` (TS's `PlayerDeathThink`) when `Killed` scheduled it, 0.1 s after a death that didn't gib. PreThink also calls it every frame while dead.
* **fill_client_data:** `stunt`, `weapons`, `rules`.
* **take_damage:** `player::damage::take_damage`, which on death calls `rules::on_killed`. That in turn calls `weapons::on_killed`. Self-inflicted damage from inside the player's own callbacks (fall, drowning, slime/lava, poison) calls `take_damage` directly, since `damage::apply` can't reach the dispatched entity.

The player's save data (`CBasePlayer::m_playerSaveData`) is mapped in `player/save.rs`.

### 4.4 Subsystems and their APIs

* **`stunt`**: `CTSStunt`/`CTSAnimation`. It sets `iuser4` bits and impulses, and picks `sequence`/`gaitsequence`. The physics they trigger runs in `shared::pm`.
* **`weapons`**: the per-player TS gun (one container, 38 weapon records), firing, hit resolution, penetration, `CBulletManager`, grenades, knives, kung fu. API for other subsystems: `give_weapon`, `give_ammo`, `ts_init`, `multiply_clip`, and TS's `weapon_tsgun` item itself (`has_gun`, `give_gun`, `remove_all_items`).
* **`rules`**: game modes, spawn selection, scoring and awards, death and respawn, power-ups (held slot, activation), slow-motion propagation, chat (`say`), cvars, TS map entities (`entities/ts`), HUD sync (`player/hud.rs`), player damage (`player/damage.rs`), drowning and time-based damage (`player/water.rs`).
* **`bot_backend`** (port addition, issue #104; `implementation/bot-plugins.md`): the `BotBackend` trait the server reaches bots through, `ts_bot_backend` (read at map start; a change reloads the map, `reload`), the quota and the fake client's join. **`plugins`**: the `tsmod_*` libraries' registry, loader rule and event callbacks.
* **`bots`**: the native bot backend, TS's bot AI, which only the Windows `mp.dll` has (decomp `server/bots.md`; `docs/implementation/rules.md` §10): fake clients created by `addbot`/`addcustombot` or the port's `ts_bot_quota` (on listen and, a chosen deviation, dedicated servers; TS has bots on listen servers only), each with a `CBotBrain` (`bots::brain`: strategy state, modules for vision, attack, buy, stunt, look around, power-up and chat, a node-graph navigator) that presses buttons and sends one `pfnRunPlayerMove` per frame, like a human's usercmd. `bots::nodes` is TS's `CNodeGraph`: the shipped `maps/*.ndm` waypoint files, node learning from every player's walk (`botlearn`), and `FindPath`; it talks to the engine through a trait, so it is unit tested against the shipped files and the decomp's replica.

### 4.5 Damage

`damage::DamageInfo` carries the inflictor, attacker, damage, `DMG_*` bits, hitgroup, direction, position and weapon id. `damage::apply(victim, &info)` dispatches to the victim's `take_damage` (TS `TakeDamage`), or, for a `traced` hit, runs TS's `ClearMultiDamage`, the victim's `trace_attack` (`TraceAttack`, which adds to `damage::multi`) and `ApplyMultiDamage` (one `take_damage` with the sum). `ShootBulletsPlayer` traces every pellet before its one Apply, so a shotgun's pellets on one victim are one hit. Hitgroup multipliers and armour follow decomp `server/weapons.md` §5.9 and `server/player-movement.md` §6.

### 4.6 Messages

* `messages::{one, one_unreliable, all, pvs, pas}(Msg, …)` return a `MsgWriter`; call `.byte()`, `.short()`, …, then `.end()`. The writer holds the writes until `.end()`, which drops a message that would stop the engine (§10).
* The text helpers (`rules::msg::{say_one, say_all, text_one, text_all, ts_message_one, ts_message_all, log}`) and the engine's `print`/`alert`/`client_print`/`log`/`set_client_key` take `&impl AsRef<[u8]>`: a `&str` works, and so do the byte strings of player text (`rules::netname` returns `Vec<u8>`; build lines with `shared::bformat!`).
* HUD values are sent from `hud::update_client_data` (TS `CBasePlayer::UpdateClientData`, 0.1 s throttle, InitHUD block included) when they change, like TS's shadow-copy approach; it ends with the gun's own `UpdateClientData` (`weapons::update_client_data`). `fullupdate` and `RemoveAllItems` (`StartObserver`, disconnect) run it inline, as TS does.

### 4.7 Time scale (slow motion)

Every entity has a time scale in `pev->fuser1` (1.0 = normal):

* Player movement multiplies frametime by it and freezes below 0.1.
* Weapon timers, animation `framerate`, and grenade and knife physics are scaled by it.
* The server eases each player's `fuser1` toward `Player::slow_target` at about 7/s, snapping within 0.05.
* Sources of slow motion: power-ups (0.35 / 0.225 for others in line of sight), global slow, slow-motion points.

### 4.8 The `unsafe` boundary

The game crates (`ts-server`, `ts-client`, `shared`) are `#![deny(unsafe_code)]`. All `unsafe`
code is in `goldsrc` (safe wrappers over the engine tables, edicts, player-move callbacks, client
hooks, native mouse) and the two `*-dll` crates (exports and callbacks, which convert raw
pointers to references). Game code keeps `*mut edict_t` as an entity handle and reads it through
`goldsrc::edict::EdictPtr` (`e.v()`). Game crates also can't use `goldsrc`'s engine-call macros
(`eng!`, `cl_eng!`, `ev!`, `efx!`, `tri!`), whose `unsafe` expansion `deny(unsafe_code)` doesn't
see; `xtask/src/lint.rs` checks for them (in `cargo test` and `cargo xtask build`), and so does
clippy's `disallowed-macros` (each game crate's `clippy.toml`, in `cargo xtask lint`). The rules for
working with this are in `AGENTS.md`.

### 4.9 Save and restore (`save/`, `goldsrc::ext::saverestore`)

Save games and level transitions write TS's bytes, so a save from TS's server loads in the port
and the other way round (as far as the entity set allows).

* **Codec** (`goldsrc::ext::saverestore`): `CSave`/`CRestore` as TS has them (the SDK's code):
  `[short size][short token][data]` records, the token being the field name's slot in the
  engine's string hash (`TokenHash`), each block led by an `int` record with its count of
  non-empty fields (`DataEmpty` over TS's `gSizes`). Times are saved relative to the save's
  time, position vectors relative to the landmark, entity references as entity-table indices.
  Fields are exchanged as `Value`s (what TS's object holds there); engine calls go through a
  `Host` trait so the bytes are unit-tested without an engine. `raw` reads and writes the
  engine's own structs for `SaveWriteFields`/`SaveReadFields`.
* **Tables** (`save/tables.rs`): every `TYPEDESCRIPTION` array of `ts_i386.so` (`gEntvarsDescription`,
  each class's `m_SaveData`, `m_playerSaveData`, `CGlobalState`'s and `gGlobalEntitySaveData`),
  generated from the binary with each field's type, count, flags and TS offset, plus each class's
  `Save` chain (from the decomp's `::Save` parents and the vtables) and the classname → class map.
  TS's chains differ from the SDK's in places (`CFuncTrackTrain` and `CBreakable` derive their
  saves straight from `CBaseEntity`, the player skips `CBaseMonster`).
* **Per-class mapping**: `Entity::save_fields` walks the class's chain with a `save::Fields`
  visitor, one call per TS field in table order, used both ways (save reads the port's state,
  restore writes it). Shared heads: `Fields::base` (`ENTVARS` + `BASE`), `delay`, `animating`,
  `stock::base::Toggle::save_chain` (down to `CBaseToggle`). Unit tests walk every linked class
  in a probe mode against its TS chain (names, field types, counts) and round-trip every class
  with all fields filled.
* **Member function pointers.** TS's g++ 2.95 member pointers are 8 bytes, delta and vtable index
  first; the `FIELD_FUNCTION` entries point at that first word (`0xffff0000` for every set
  non-virtual function), which no engine can name. `WriteFunction` logs "Invalid function pointer
  in entity!" and writes nothing, but the field was counted, so on restore the next block's header
  is consumed as a field and the rest of that entity's chain isn't read. The port passes the same
  word to `pfnNameForFunction` and so writes the same bytes; classes only say whether each of TS's
  pointers would be set.
* **Engine callbacks**: `DispatchSave` (table entry, `FCAP_DONT_SAVE`, push-entity `ltime`),
  `DispatchRestore` (global entities restored over the live one in global mode with the landmark
  offset moved by the `mins`, then `Precache`; `gGlobalState` checks), `SaveGlobalState` /
  `RestoreGlobalState` (`"GLOBAL"` + `"GENT"` blocks), `ResetGlobalState` (also `gInitHUD`).
  `ObjectCaps` comes from the classname table `levelchange::ts_object_caps`.

## 5. Map entities

Census of the 97 shipped maps: how many maps use each class. Implement in this order.

| Priority | Classes |
|---|---|
| 1 (most maps) | `worldspawn`, `func_wall`, `info_player_deathmatch`, `func_illusionary`, `light`, `func_breakable`, `light_environment`, `func_ladder`, `info_player_start`, `ambient_generic`, `env_sprite`, `ts_powerup`, `trigger_hurt`, `trigger_multiple`, `func_button`, `func_door_rotating`, `multi_manager`, `ts_model`, `func_water`, `ts_groundweapon`, `func_door`, `info_target`, `env_glow`, `light_spot`, `infodecal` |
| 2 | `trigger_teleport`, `trigger_auto`, `info_teleport_destination`, `func_rotating`, `trigger_push`, `func_train`, `path_corner`, `func_conveyor`, `item_healthkit`, `ts_mapglobals`, `func_wall_toggle`, `trigger_camera`, `trigger_changetarget`, `trigger_relay`, `cycler_sprite`, `info_null`, `env_shake`, `env_explosion`, `multisource`, `trigger_once`, `item_battery`, `env_render`, `func_pushable`, `env_beam`, `info_player_teamspawn`, `ts_groundammo`, `game_text`, `env_fade`, `env_shooter` |
| 3 | HL weapon/ammo names that TS re-points at ground items (`weapon_*`, `ammo_*`: decomp `server/architecture.md` §5), `ts_emitter`/`ts_particle`/`ts_fog` (the client reads them; the server validates them and removes all but the particles), the remaining TS objective entities (stubs: they're dead in 3.0) |

Unknown classnames make the engine print "No spawn function" and free the entity, so maps
still load.

## 6. Client architecture

`client-dll/src/lib.rs` exports the individual entry points (`Initialize`, `HUD_*`, `IN_*`,
`CL_CreateMove`, `V_CalcRefdef`, `KB_Find`, …), converts the engine's pointers, and delegates to
`ts-client`. It also holds `tempents.rs`, the SDK temp-entity loop over the engine's linked list,
and `vgui.rs`: the `ClientFactory`/`CreateInterface` exports and an `IClientVGUI` object
(`UseVGUI1() == false`). Steam GoldSrc needs it to draw the world, because the port has no VGUI1
viewport to call `VGui_ViewportPaintBackground` as TS's does (issue #62); FWGS ignores it.

| Module | Contents | Package |
|---|---|---|
| `input` | kbuttons (`+alt1` = stunt `IN_ALT1`, `+alt2` = 0x40), `CL_CreateMove`, the anti-script gate | HUD/UI |
| `hud` | message hooks (all 64 must be hooked), HUD state, one immediate-mode draw pass in TS order | HUD/UI |
| `menus` | buy menu (TS's own immediate-mode menu, `tsbuylist.tsa`); TS's VGUI1 team menu, scoreboard and MOTD window, painted with engine fills through `hud::vgui` | HUD/UI |
| `voice` | `CVoiceStatus`: talking players' head icons and labels, the talk icon, scoreboard voice icons and squelch-mode muting, `voice_ban.dt`, `VModEnable`/`vban` | HUD/UI |
| `joystick` | the SDK joystick code TS ships (`IN_JoyMove`, `IN_Commands`), Steam GoldSrc only; devices are read by `goldsrc::joystick` (WinMM / SDL2) | HUD/UI |
| `view` | `V_CalcRefdef`: bob, punch, client-side recoil through `cl_viewangles`, slow-motion tint and `TSFade` | rendering |
| `camera` | third-person camera controls (`CAM_Init`'s `+cam*` commands, `snapto`, the `CAM_Think` mouse and key adjustments); `view` owns `cam_thirdperson` and the `thirdperson` toggle | rendering |
| `demo` | client data in demos (`Demo_WriteBuffer` / `Demo_ReadBuffer`): the zoom target (type 0), the alternate grip (type 1) and the `thirdperson` state (type 2) | rendering |
| `events` | `fire.sc`, `empty.sc`, … handlers: sounds, muzzle flash, shells, impacts, decals | rendering |
| `prediction` | `HUD_PostRunCmd` weapon prediction, Txfer callbacks carrying `iuser4`/`fuser1`/`vuser*` | rendering |
| `effects` | temp entities, TS particle system, BSP-lump map emitters, laser sights, motion-blur ghosts | rendering |
| `spectator` | `CHudSpectator` (HLTV director events, `specmode`) and the spectator view: in-eye camera and gun, `cl_vsmoothing` | rendering |
| `studio` | the TS studio model renderer (`HUD_GetStudioModelInterface`): split torso/legs bones, gait, corpses, ghosts | rendering |
| `state` | values both sides read: time scale, health, dead, weapon, fov | shared |
| `relaunch` | port addition: restart and reconnect (`implementation/client-plugins.md` §13.5): launcher IPC, `ts-launcher relaunch`, self-relaunch where verified, the quit-and-inform / reconnect / password cards (SDL windows of their own), `.reconnect`, `tsrs_reconnect`, `ts_dev_restart` | port |

`HUD_GetStudioModelInterface` installs the TS studio renderer (`studio`), so every studio model
is drawn by the client: the SDK `CStudioModelRenderer` with TS's split torso/legs bones, gait and
renderfx 17 corpses and motion-blur ghosts. The engine side is `goldsrc::ext::studio`.

## 7. Shared crate

* `pm`: TS player movement, an exact port of the server's `pm_shared` with the TS changes (decomp `server/player-movement.md` §1). The client's copy in `client.dll` (and `mp.dll`'s) has the same logic apart from `PM_SpectatorMove` (HLTV camera additions, see `docs/implementation/client-rendering.md`), so one implementation serves both sides. The floating point differs, though: `ts_i386.so` is GCC at x87 extended precision, `client.dll`/`mp.dll` are MSVC at 53 bits with other rounding points, so the code is generic over the build (`pm::Arith`): client prediction uses `Arith::CLIENT` (`client.dll`'s), and the server `mp.dll`'s on every platform (`Arith::SERVER_DEFAULT`, the same arithmetic, so server and prediction agree exactly) unless the port's `ts_pm_arith gcc` cvar selects `ts_i386.so`'s (§10). See `docs/implementation/movement.md`.
* `weapons`: `tables()` parses the embedded TOML once. The predicted gun state machine (fire modes, timers × `fuser1`, spread penalty, seeded pellets) goes here.
* `protocol`: the message table and per-area payload encoders and decoders.
* `ts`: stunt bits, power-up bits, observer modes (TS numbering: in-eye is 5), `IN_ALT1`, the slot/speed formula.
* `gameplay`: the movement and gun hooks (`PmRules`, `GunRules`) of the gameplay plugins (TS 3.0's in `crates/ts30`, TS 2.1's in `crates/ts21`). Movement takes the active rules as it takes `Arith`, and the gun reads them from `GunHost::rules`. The server and client crates define their own hook traits (`ServerRules`, `ClientRules`), and `ts_gameplay` picks the ruleset at each map start (a mid-map change of it or of `ts_bot_backend` reloads the map: `ts_server::reload`). See `docs/implementation/gameplay-plugins.md`.
* `bsp`: the BSP entity lump and its keyvalue blocks (the client's map effects and spectator cameras).
* `protocol::port`: what tells the port's libraries from TS 3.0's on channels TS 3.0 tolerates: the client's `_tsrs` userinfo key (its version, `3.0.2` or the numeric `3000002` a `+connect` sends first; in 2.1 mode the server refuses clients without it, and `sv_client_version`/`sv_client_require_port` can refuse by version: `ts_server::rules::client_version`, `implementation/client-plugins.md` §13.1) and the server's version in `SrvSett` keys 5..7 (the client prints a difference; `cl_version_strict 1` disconnects).
* `protocol::ext`: `TSExt`, the port's own user message after TS's 64 (`protocol::TS_MESSAGES`): a channel and a payload for the port client's extensions, at most 192 bytes, sent only to clients that hook it (`_tsrs` at or above `port::TSPKG_SINCE`, `TSPkg`'s gate) (plugin API version 8's `ext_message`; `implementation/client-ext-api.md` §10).

## 8. Build targets and binary compatibility

| Target | Installed as | Used by |
|---|---|---|
| `i686-unknown-linux-gnu` | `dlls/ts_i386.so`, `cl_dlls/client.so` | ReHLDS, HLDS (Linux), Steam HL Linux |
| `i686-pc-windows-gnu` | `dlls/mp.dll`, `cl_dlls/client.dll` | ReHLDS, HLDS (Windows), Steam HL Windows |
| `x86_64-unknown-linux-gnu` | `dlls/ts_amd64.so`, `cl_dlls/client_amd64.so` | FWGS 64-bit |
| `x86_64-pc-windows-gnu` | `dlls/mp_amd64.dll`, `cl_dlls/client_amd64.dll` | FWGS 64-bit Windows |

* **Bindings** are generated per target from the FWGS headers, which `_Static_assert` the 32-bit layouts.
* **Windows** builds use MinGW-w64 (UCRT). `GiveFnptrsToDll` is exported undecorated, and the only imports are system DLLs (`api-ms-win-crt-*`, present on Windows 10+ and Wine).
* **Linux i686** builds currently need glibc 2.34. Building against an older glibc for old HLDS hosts (`cargo zigbuild`) is a later step.
* **Windows export section.** Steam's engine finds the export names by looking for the section named
  `.rdata`, as MSVC lays it out. MinGW's `.edata` breaks that, so the xtask renames the sections of
  every Windows DLL it installs or packages (see `docs/validation.md` §4). DLLs straight out of
  `cargo build` are not patched: use `cargo xtask install`/`dist`.
* `panic = "abort"`. A panic hook writes `ts_server_panic.log` / `ts_client_panic.log`, then the
  whole engine process ends, so game code must not panic on anything the engine, a map, a model,
  a save, a cvar or a client can feed it. `catch_unwind` at the export boundaries is no help
  under `abort` (nothing unwinds to catch), and unwinding across `extern "C"` is undefined, so the
  strategy stays: guard the input instead (checked indexing, no `RefCell` borrow held across a
  call that can borrow it again, wrapping C arithmetic where TS's C would wrap).
* **Integer overflow wraps** in every installed build. `cargo xtask install` defaults to the dev
  profile, which has `overflow-checks = false` so that an overflow does what TS's C (and the
  release profile) does instead of aborting; `cargo test` keeps the checks (`profile.test`).
  Division by zero and `i32::MIN / -1` still panic in every profile, so divisors from data are
  guarded.

## 9. Validation

| Root | Engine | Path (default) |
|---|---|---|
| `fwgs` | Xash3D FWGS, amd64 (`xash` dedicated, `xash3d` client) | `~/games/xash` |
| `rehlds-linux` | ReHLDS (built from `../ReHLDS`) on HLDS `steam_legacy` | `~/games/hlds_linux` |
| `rehlds-win` | ReHLDS 3.15 release on HLDS `steam_legacy`, under Wine | `~/games/hlds_win` |
| `hlds25-linux` | stock HLDS, post-anniversary (`public` branch) | `~/games/hlds25_linux` |
| `hlds25-win` | stock HLDS, post-anniversary, under Wine | `~/games/hlds25_win` |
| `fwgs-win` | Xash3D FWGS 0.21 win32 (from `xash3d-ts`), under Wine | `~/games/xash_win` |

The `cargo xtask` commands:

* `cargo xtask install all` assembles `<root>/ts`. It symlinks the TS content directories and copies config files, so the local TS install is never modified. Then it installs `game/ts` and the libraries for that root's target.
* `cargo xtask server <root> --bots N` starts the dedicated server and checks:
  * that it answers A2S_INFO with the right map,
  * that the bots joined,
  * that the log has the Rust `GameInit` line and no panic or fatal error.
* `cargo xtask test` runs the same check on every root.
* `cargo xtask client <root>` runs that root's dedicated server and joins it with the FWGS
  client and this client library (GoldSrc protocol for ReHLDS and HLDS), saving a screenshot.

The FWGS client can join GoldSrc-protocol servers (`connect host:port gs`), so one FWGS client
with this client library validates the full chain against ReHLDS and HLDS. See
`docs/validation.md` for results.

Test maps: `ts_arena` (6 classes, smoke test), `ts_awaken` and `ts_bay107` (power-ups, ground
weapons), and `ts_lobby` (19 classes, the classic).

## 10. Deliberate deviations from TS 3.0

The port matches TS, including its bugs. The exceptions are out-of-bounds reads and writes that
only happen on bad input, which are guarded instead (each implementation doc lists them):

* the scoreboard overrun (74 writes into a 64-byte array)
* the always-true objective bounds checks

The port also never crashes, hangs or stops the engine where TS does; it does the least that
keeps the game running and logs it ("(port guard)"):

* Engine calls the Steam engines answer with `Host_Error`/`Sys_Error` (empty, sentence, late,
  missing-file or over-limit precaches, unprecached `SET_MODEL`/`MODEL_INDEX`, out-of-range
  sound parameters, backwards `SET_SIZE`, too-long or mis-sized user messages, `MSG_ONE`/`SET_VIEW`
  to a non-client) and those every engine stops on (light styles past 63, no free edict, a bad
  `WRITE_ENTITY`, a `*n` brush model the map's BSP doesn't have) are checked in `goldsrc`
  (`guard.rs`) first; on FWGS, which only warns about the former, they are made as TS makes them.
  The list is in `implementation/integration.md` §6; the respawned `cycler_weapon`'s empty
  precache is one TS reaches.
* Client engine calls that stop or crash the client engine (Steam GoldSrc, read from the HL25
  `hw.so`, and FWGS): the 257th HUD sprite, sound names over 63 bytes, TriAPI on a null sprite,
  overview images that aren't `.bmp`/`.tga`, negative model indices, studio body parts the
  renderer would read out of bounds or divide by zero with, missing sequence group files, bad
  event indices, decals on unknown entities and user-message hooks past FWGS's limit. They are
  checked in `goldsrc` (`cl_guard.rs`) and logged to the console once each:
  `implementation/integration.md` §6.1.
* Entity states the physics stops on (a `SOLID_BSP` entity that isn't a pusher with a brush
  model, a map `movetype` without a physics case), `Use` chains that recurse back into
  themselves, and `Q_log2` of a negative number, which never returns in TS:
  `implementation/stock-entities.md` §7.8.
* The client's `exit(-1)` on a model with more than 4 attachments
  (`implementation/client-rendering.md`).

Server plugins get a port-only API instead of TS's object layout: `CreateInterface` hands AMX Mod
X's TS module a table of functions over the port's player state, and the private-data block only
looks like TS's object as far as keeping old `get_pdata`/`set_pdata` offsets in bounds
(`implementation/plugin-api.md`). Version 2 of the table adds entity hooks for a port of AMX Mod
X's hamsandwich, which can't patch vtables the port doesn't have: the game's entity functions go
through `ts_server::ham`, which calls the plugin's hook before and after each watched one
(numbered by hamsandwich's `Ham` enum; the pre hook with the object still in its slot, the post
hook once it is back, or around a lend for calls inside the object's own dispatch) and runs
them on request (`ExecuteHam`/`ExecuteHamB`). Unwatched, each entry point costs one bit test
(`implementation/plugin-api.md` §7).

One behaviour follows TS's Windows build on every platform:

* **Player movement (and the weapon maths both sides share) on a Linux server uses `mp.dll`'s
  arithmetic.** TS's Linux `ts_i386.so` (GCC, x87 extended precision) and its Windows
  `mp.dll`/`client.dll` (MSVC, 53-bit) round movement differently in the last bits, so TS's
  client mispredicted its own Linux server (a duck finished at 600 ms on the client, 590 ms on
  the server). The port's server runs the
  Windows build's arithmetic everywhere by default, which every client (TS's and the port's)
  predicts exactly. The port's server cvar `ts_pm_arith gcc` restores `ts_i386.so`'s arithmetic
  bit for bit (`implementation/movement.md`, `implementation/weapons.md` §5).

Dead 3.0 features (round-based and objective modes, cash) are implemented only as far as the
port needs them; `docs/implementation/*.md` records what's live.
