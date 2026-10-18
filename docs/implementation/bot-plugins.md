# Bot plugins

Issue #104. The bots are a selectable layer of their own, separate from the gameplay rulesets
([gameplay-plugins.md](gameplay-plugins.md)). The server reaches bots only through a **bot
backend**:

* **`native`** (the default): TS 3.0's bots, the AI ported from `mp.dll` (`crates/server/src/bots`,
  [rules.md](rules.md) §10, decomp `docs/server/bots.md`).
* **a plugin backend**: a `tsmod_*` library the server loads ([plugin-api.md](plugin-api.md) §1.2)
  that registers a backend under a name. The first is `tsmod_rcbot`, the Rust port of RCBot1 in
  [the-specialists-mod/tsmod-rcbot-rs](https://github.com/the-specialists-mod/tsmod-rcbot-rs).
  RCBot is GPL, so it is never linked into the server; it uses the same plugin API as the AMX Mod
  X modules. The in-repo `tsmod_test` (`crates/tsmod-test`) is a stub backend for the tests.
* **an attached plugin's backend** (plugin API version 7, #290): a metamod plugin, such as an AMX
  Mod X module, that took a plugin identity with `attach_plugin` ([plugin-api.md](plugin-api.md)
  §1.4) registers a backend as a loaded plugin does. This is how an AMX Mod X-hosted bot (a Pawn
  plugin on Rust AMXX modules, `the-specialists-mod/tsmod-amxx-rcbot`) becomes
  `ts_bot_backend rcbot_amxx` (§3.1).

The whole layer is a port addition: TS has one hard-wired bot AI, on listen servers only.

## 1. Layout

| Part | Where |
|---|---|
| `BotBackend` trait, registry, `ts_bot_backend`, the bot player (`BotSetup`), the owners table | `crates/server/src/bot_backend/mod.rs` |
| `ts_bot_quota` and the bot manager's timing | `bot_backend/quota.rs` |
| The native backend | `crates/server/src/bots/backend.rs` (`Native`), calling the functions in `bots/` |
| The native bots' rules per gameplay ruleset (§2.2) | `crates/server/src/bots/rules.rs` (`NativeBotRules`, the registry); TS 3.0's in `crates/ts30/server/src/bots/` (with 3.0's state think and vision, buy, look-around and attack modules), TS 2.1's in `crates/ts21/server/src/bots/` |
| A plugin's backend (C callbacks → `BotBackend`) | `crates/server-dll/src/plugins.rs` (`PluginBackend`) |
| The API's bot functions | `crates/server/src/plugin_api/bots.rs` |

## 2. The `BotBackend` trait

```rust
pub trait BotBackend {
    fn name(&self) -> &str;
    fn add_bot(&self, req: &AddBot) -> Option<(*mut edict_t, Option<f32>)>;  // Addbot / Custom / Quota
    // all below default to nothing
    fn game_init(&self); fn map_init(&self); fn precache(&self);
    fn server_activate(&self); fn server_deactivate(&self);
    fn activate(&self); fn deactivate(&self); fn think(&self);
    fn new_state(&self, skill: f32) -> BotState;          // default: the plugin marker
    fn adopted(&self, e: *mut edict_t);
    fn spawn_pre(&self, p, cx); fn spawn_post(&self, p, cx);
    fn trace_attack(&self, p, cx, dmg); fn killed(&self, p, cx, attacker);
    fn on_kill(&self, killer, victim, now); fn pre_think(&self, p, cx);
    fn client_command(&self, p, cx, cmd, args) -> bool;
}
```

The per-player hooks take the `Player` and run inside its dispatch, at the points the native
bots ran before, so TS's order of calls is kept exactly. A plugin backend implements only
`name`, `add_bot`, `activate`, `deactivate` and `adopted`: plugins hear the game's events through
their own callbacks (plugin-api.md §1.3), at points where the player is in reach.

A bot's backend state lives on its `Player` (`Player::bot: Option<BotState>`, `BotState =
Box<dyn Any>`): the native backend keeps its `Brain` there (`bots::brain_of`), a plugin bot a
`PluginBot` marker (the plugin keeps its own state). `Player::cbot` (the `CBot` class:
`IsNetClient()` false, no starting cash, the vote line) and `Player::is_bot` (`m_bIsBot`) are TS's
player fields and are set for every backend's bots.

### 2.1 The coupling points

Each of the 15 places outside `src/bots` that called the native bots now calls the active
backend through `bot_backend` (no behaviour change for native):

| Site | Now |
|---|---|
| `server-dll` `GameDLLInit` | `bot_backend::game_init` (registers native, first in the list even when an attached plugin registered a backend before, then its `game_init`) |
| `server-dll` `StartFrame` | `bot_backend::start_frame` (the host's quota, then `think`) |
| `rules::start_rules` (`CTSGameRules::Init`) | `bot_backend::map_init` (native: nodes and bot texts) |
| `rules::mapcycle` (the map end's `Init`) | `bot_backend::map_init` |
| `rules::server_activate` / `server_deactivate` | `bot_backend::server_activate` / `server_deactivate` (the quota's timing, then the backend) |
| `Player::bot` | `Option<BotState>` (above) |
| `Player::spawn` head / tail | `bot_backend::spawn_pre` / `spawn_post` |
| `Player::trace_attack` | `bot_backend::trace_attack` |
| `Player::pre_think` (node learning) | `bot_backend::pre_think` |
| `rules::lifecycle` `Killed` | `bot_backend::killed` |
| `rules::scoring` and the 2.1 plugin's `scoring` (the kill taunt) | `bot_backend::on_kill` (still only for a kill that scored) |
| `commands` (node and `testpath` commands) | `bot_backend::client_command` |
| `rules::commands` `addbot` / `addcustombot` | `bot_backend::add_bot(AddBot::Addbot / Custom)` |
| `precache` (`sprites/lgtning.spr`) | `bot_backend::precache`, at the same place in the precache order |

Kept as they were, because they are TS's player state and TS's cvars rather than the AI's:
`Player::is_bot`'s clear in `TSPlayerDisconnect`, `next_node_learn` (`pl+0x788`, `TSInit` zeroes
it), the `FL_FAKECLIENT` from `is_bot` at spawn, and the cvars (`botlearn`, `bottalk`, `botarmor`
are in TS's cvar list; `ts_bot_quota`, `ts_bot_stop`, `ts_bot_backend` are the host's).

### 2.2 Rulesets and the native bots

A gameplay ruleset can change how TS's own bots play (TS 2.1's do: #100 §3.10). That is the
native backend's business, not the host's and not the gameplay plugin's:

* **`NativeBotRules`** (`bots/rules.rs`) is the native backend's hook trait. Its methods have
  no default bodies: a ruleset's bots implement all 29 (TS 3.0's AI is `ts30_server::bots::BOTS`,
  with the 3.0 state think and modules behind it; issue #200). Hooks: creation (`random_model_in_teamplay`, `learn_on_empty_graph`,
  `stored_skill`, `team_key`, `texts`), the `CBot` overrides (`spawn_post`, `trace_attack`), the
  brain (`delay` for every module timer, the states and the stuck window; `runs_ai`, `turn_rate`,
  `update_view`, `look_at`, `press` / `release` / `update_buttons`, `respawn`), navigation (`ladder_buttons`, `wander_end`, `steer_probe_end`, `nav_distance_order`, `forward_detour`) and whole modules
  (`state_think`, `vision`, `attack`, `buy`, `look_around`, `stunt_mode`), and the node graph's
  link arithmetic (`ladder_link_distance`, `IsReachable`'s ladder shortcuts; `ground_probes`,
  `IsGroundContinuous`'s floor probes). Coarse where a ruleset
  replaces a module, fine where it changes one value inside shared code (the timers, the view,
  the buttons). A replacement gets the same `Brain`, `Player` and `Cx` and uses the native
  helpers, which are public; `Brain::ext` holds per-bot state of its own (Reset drops it).
* **Its own registry, keyed by the gameplay id.** The server library registers a ruleset's rules
  with `bots::rules::register(id, &RULES)` next to the gameplay plugin; `bots::rules::current()`
  resolves them from `gameplay::active_id()` (cached per id), and an id without rules runs
  the first registered (3.0's when it is built in). So `ts_gameplay` picks the bots' rules and `ts_bot_backend` picks the backend,
  independently: a plugin backend never sees these hooks, `ServerRules` keeps no bot hooks, and
  `crates/server` names no ruleset (the isolation lint).
* Why not a field of `ServerPlugin` or hooks in `ServerRules`: the bot layer is a separate axis
  (this document), and putting the native AI's internals (`Brain`, its modules) into the gameplay
  plugin's interface would tie every ruleset to one backend. Why not a backend of its own (a
  second "native 2.1" backend): `ts_bot_backend` is the user's choice of AI, and the gameplay
  ruleset must not change it (§3); the native backend follows each map's ruleset through the
  per-call resolution (`server_activate` loads the ruleset's bot texts if `Init` loaded others).

The host's `create_player` (`BotSetup`) takes `team_key`, because 2.1's `BotSetup` has no `team`
key; the native backend passes its rules' choice and plugin bots get 3.0's.

## 3. Selection: `ts_bot_backend`

Port cvar, default `native`. **Read at map start** (`rules::install`, right after the synchronous
`exec game.cfg` and before the rules' `Init`): `bot_backend::map_start`, through
`bot_backend::wanted`. So:

* `game.cfg` selects it for the map being loaded.
* **A change reloads the map**, from any source (the console, rcon, the server config, a script):
  when the backend `ts_bot_backend` resolves to differs from the running one at one frame and the
  next, the console says `[ts] bots: backend <name>: reloading <map>` and the server changes level
  to the same map (`crate::reload`, shared with `ts_gameplay`: one reload when both change,
  none for a change and back, no loop; [gameplay-plugins.md](gameplay-plugins.md) §4). The
  server config (`servercfgfile`) runs after the map has loaded, so on the Steam engines a
  backend it sets reloads the first map once; FWGS's dedicated server also runs it before the
  first map, which then starts with that backend and needs no reload. When it runs again on a
  later map nothing changes.
* Names are compared case-insensitively; empty means native. An unknown name warns once per
  value (`[ts] bots: unknown ts_bot_backend "x", using native`) and native runs. The reload
  compares the resolved backend, so `NATIVE` while native runs, or an unknown name, reloads
  nothing.
* Each change logs `[ts] bots: backend <name>`, calls the old backend's `deactivate`, **kicks
  every bot the old backend made** (`kick #<userid>`, queued) and calls the new one's
  `activate`. The quota then adds the new backend's bots. The gameplay ruleset (#100) doesn't change
  the backend.

### 3.1 Backends of attached plugins (version 7)

An AMX Mod X-hosted backend registers at one of two points, and both work without an extra map
reload:

* **At `Meta_Attach` / `AMXX_Attach`** (an AMXX module): before the game's `GameDLLInit`, so before
  native registers. Native is still put first (the default and the fallback), and the first map
  start selects the attached backend like any other when `ts_bot_backend` names it (FWGS's
  dedicated server runs the server config before the first map; on the Steam engines the config
  runs after it, which reloads the first map once, as for a `tsmod_*` backend).
* **In a Pawn `plugin_init`**: AMX Mod X runs it from metamod's `ServerActivate` post hook, after
  `bot_backend::map_start` chose the map's backend (in `CWorld::Precache`) and after the bot
  manager's `ServerActivate`. A name that wasn't registered then resolved to native (with the
  "unknown `ts_bot_backend`" warning). `bot_backend::register_plugin` therefore checks, once a map
  start has run: if native runs and `ts_bot_backend` now resolves to the new backend, it switches
  at once, as a map start would (`switch_to`: native's `deactivate`, its bots kicked, `[ts] bots:
  backend <name>`, the new one's `activate`); the quota then adds the new backend's bots. The
  reload watcher then sees the running backend matching the cvar and reloads nothing. On later
  maps the backend is registered already and the map start selects it.

`create_bot` takes the attached plugin's id. When the plugin detaches (`detach_plugin`), its
backend is unregistered; if it was active, it is deactivated, its bots kicked and native
activated mid-map (`bot_backend::drop_plugin`). A `ts_bot_backend` still naming it then resolves
to native, so nothing reloads.

## 4. Who owns what

* **The host** owns the quota (`ts_bot_quota`: names `Bot Neo`, ..., one add or kick per half
  second, a second after `ServerActivate`), `addbot` / `addcustombot` (they ask the backend:
  `AddBot`), the fake client and its join (`bot_backend::create_player`, TS's `BotSetup`), the
  owners table (which backend made which bot, by user id) and `ts_bot_stop` for plugin bots
  (`run_cmd` clears a living bot's movement and buttons).
* **The backend** picks names, models and skills (native: `bots.txt`, TS's seven models),
  thinks, and drives its bots.
* A quota bot the engine kept across a level change (a new `Player`, no state) gets
  `new_state(1.0)` and `adopted` (plugins: `BotBackend::adopt_bot`).

## 5. Writing a plugin backend

1. Export `TSRS_PluginLoad` (plugin-api.md §1.2); check `api->version == 7` (`ts_plugin_api::VERSION`).
   A metamod plugin instead gets the table from `CreateInterface` and its id from
   `attach_plugin("<name>")` (plugin-api.md §1.4), and hears the game through a listener it owns
   (`add_listener(id, &descriptor)`).
2. In it (or in `game_init`): `register_bot_backend(id, "rcbot", &backend)` with `add_bot`,
   `activate`, `deactivate`, `adopt_bot`. Register commands (`rcbot ...`) and cvars, read data
   from `data_dir(id)` (`<gamedir>/tsmod/<name>/`: waypoints, learned data).
3. `add_bot(name, model, skill)`: pick what is empty, call `create_bot(id, name, model, skill)`,
   keep the index, return it. The strings are bytes as typed (not UTF-8): pass a given name and
   model on unconverted, or a CP1252 byte turns into U+FFFD's three bytes (#190).
4. Every `start_frame` while active: read the frame once (version 6: `players` for every slot,
   `frame_info`, `items`; read `player_inventory` again only when a `PlayerSnapshot`'s
   `inventory_serial` moved; version 7: the snapshot's `KUNG_FU` / `SUPERJUMP` flags carry the
   permanent power-ups, `FrameInfo::team_scores` the team scores), then for each bot think and call `run_cmd` once with
   `cmd::SET_VIEW | cmd::AUTO_MSEC`: the server turns the bot as its own bots turn, picks `msec`
   from the frame time, and reports the result. Adopt `BotCmdResult::v_angle` after a
   `FIXANGLE` event (a spawn or teleport turned the bot), drop the path after `TELEPORTED`.
   Read the rest with the engine's own functions (`engine_funcs`: traces, PVS) and the API
   (`entity_info`, `game_state` with the team scores, `objectives` for the HUD's objective list,
   `master_triggered`, `entity_key` for a door's or button's `master`).
5. Act through the port's code paths: `select_weapon` (TS 3.0 ignores `usercmd.weaponselect`),
   `buy`/`buy_item` (the loadout bought at the next spawn), `bot_command` (`usepowerup`,
   `respawn`, `say`), `drop_weapon`, `kick_bot`. Make an entity from keys as a map does with
   `create_entity`, `entity_key_value` and `spawn_entity` (RCBot's `givetsweapon`: a
   `ts_groundweapon` with `tsweaponid`, then `ham_execute(Ham_Use)` with the bot as activator).
6. Hear the game through the descriptor's callbacks: spawns, damage (`DamageEvent`: who, where,
   with what), deaths, kills, rounds, sounds (hearing; with the channel and the player
   responsible), shots (`weapon_fired`), touches (`entity_touch`: triggers, pickups), pickups
   (`item_pickup`), weapon and ammo changes, client commands (`rcbot` typed by a player).

RCBot's needs from the research (issue #104's comment) map as: its engine hooks → the
callbacks; message sniffing → `player_info`/`player_inventory`/`weapon_changed`/`ammo_changed`;
`FakeClientCommand` → `bot_command`; `weaponselect` → `select_weapon`; the free-weapon loadout →
`buy`; model-string teams → `PlayerInfo::team`; `IsTriggered`/`Classify` virtuals →
`master_triggered`/`entity_info`; `pfnRunPlayerMove` → `run_cmd`; `rcbot/waypoints/ts` →
`data_dir`. Version 4 (issue #117) added what RCBot still worked around: the `TeamScore` values
(`GameState::team_scores`, its GA's death fitness), the `Objective` messages' list
(`objectives`: id, origin, name and whether it is active) and entities made from keys
(`UTIL_makeTSweapon`, `ExplosionCreate`). Version 6 added what it still estimated or scanned for:
the view and `msec` handling of the native bots and the move's result (`run_cmd`), one snapshot of
every player and of the items per frame, the combat timers (when a player can attack, stunt or
get up, and what it blocks), where a hit landed, shots, touches, pickups, and the `master` keys
of the map's entities.

## 6. Tests

* Unit (`cargo test`): the native rules' 3.0 defaults (`bots::rules`) and TS 2.1's overrides (`ts21_server::bots::tests`: timers, gate, view, button slots, texts, buy and state helpers), backend selection, quota names, the loader (plugin-api.md §6), the API's
  bot checks, the stub's descriptor; version 7: an attached plugin's backend (registered before
  native, which stays the default; gone after `detach_plugin`) and when a late backend takes over
  (`plugins::tests`, `bot_backend::tests`).
* `cargo xtask plugins [roots...]` (all seven roots by default, in parallel, ~1 min): installs
  `tsmod_test` and fixtures the loader must skip (`tsmod_test_badver`, `_badsize`, `_refuse`, a
  copy of the server library as `tsmod_noexport`, the other architecture's build as
  `tsmod_wrongarch`, junk, another platform's name), runs a dedicated server on ts_dojo whose
  first config (the server config, on the first map) sets `sv_cheats 1` (for the stub's `powerup`
  cheat; TS latches it at a map's start) and `ts_bot_backend test` (one reload on the
  Steam engines, none on FWGS, which ran the config before the first map; no `changelevel`) and `ts_gameplay` 21 and back (no reload), and whose second (`ts_bot_backend
  TEST`: no reload) adds bots with
  the quota, `addbot` and `addcustombot`. Checks: each skip line, `loaded tsmod_test`, `backend
  test`, at most one `reloading` line and only the backend's, and the stub's `check <what> ok` lines (data_dir, map_name, create_bot, run_cmd,
  player_info, entity_info, game_state, team_scores, objectives, create_entity (an `info_target`
  from keys, spawned at its `origin`, and an unknown class refused), gun_hook (`Ham_Item_PreFrame`
  on a bot's `weapon_tsgun`), inventory, bot_command, client_command, spawned, buy, buy_item,
  moved more than 64 units, sound; plugin API version 5, #198: add_listener (a listener
  registered in `game_init`, a short descriptor refused), listener (its `player_spawned` heard
  for a bot of ours), remove_listener (removed once heard, a second removal refused, never heard
  again), private_size (a bot's > 0, -1 refused), player_stunt (the bots press the stunt key
  every 2.5 s while running: a dive), player_powerup (each bot runs `powerup 5` then
  `usepowerup`: double fire rate), melee_attack (the bots hold attack while they have kung fu
  out, in their first life), melee_attack_change (the stub answers a move with a 2 s delay and 1
  damage; the next move must come at least 1.6 s later) and melee_attack_block (it blocks the
  next with a 1.5 s delay; the one after must come at least 1.2 s later), ham_take_damage (12 s
  into the backend's run one bot kills another with `Ham_TakeDamage`, 1000 damage, by its own
  hand) and kill_scored (that kill reported: the killer as inflictor, at least a point, a killer
  spree of at least 1, a weapon id and name; the stub logs the whole report as
  `kill_scored: killer .. victim .. ...`); plugin API version 6: players_count and players
  (every slot, a bot's agreeing with `player_info` and the engine), frame_info (against
  `game_state` and the globals), items (against `entity_info`), inventory_serial (moved with a
  changed `player_inventory`), combat_power and combat (the next attack ahead after a kung-fu
  move), run_cmd_time (the game time put back), run_cmd_result, cmd_fixangle (the spawn's view,
  adopted), set_view (the command's view kept afterwards), cmd_landed, cmd_teleported (the
  scripted kill's victim respawned), entity_key (a `master` key on the `info_target`), sound_source,
  entity_touch, damage (the scripted kill as a `DamageEvent`), weapon_fired (once the kung-fu
  checks are done each bot gets an M4 with `ts_give`, which needs `sv_cheats`, and pulls the
  trigger every second) and item_pickup (a `ts_groundweapon` Beretta spawned beside the bot and
  handed over 2 s later with `Ham_Use`); plugin API version 7, #290: attach_plugin (in
  `game_init` the stub attaches `test_attached` as an AMX Mod X module would: bad and taken names
  refused, its own loaded name included; the data directory `tsmod/test_attached/`, a command, a
  bot backend and a listener of its own), detach_plugin (the id is dead for `data_dir` and
  `register_bot_backend`, its listener is gone, a loaded plugin's id and `NO_PLUGIN` are refused,
  attaching again gives a new id) and snapshot_powers (a bot's `KUNG_FU` / `SUPERJUMP` flags
  against `player_info`), with `frame_info`'s team scores checked against `game_state`); plugin
  API version 8, #294/#304: ext_message (a bot isn't a port client, a broadcast reaches nobody, a
  bad channel, flag or length and a NULL payload are refused, the longest payload fits) and
  client_port_version (a bot has none; slot 0 isn't a player); #321: offer_client_plugin (an id
  with no checked offer and NULL are refused before the bot's client, slot -1 isn't a player) and
  client_plugin_version (a bot runs no client plugins, NULL is refused, slot 0 isn't a player), no
  `FAIL`, no crash, and no "server tried to get non-existent cvar" warning from FWGS for the
  server's and plugins' new cvars (#331). The fixtures are removed
  afterwards and by every `install`, so other scenarios run without plugins.
* Native parity: `cargo xtask bots`, `play`, `test` unchanged.

## 7. Deviations and limits

* The layer, the cvar and the API are port additions.
* Plugin bots count as `CBot`s (`IsNetClient()` false, no starting cash), as TS's bots do.
* A backend switch reloads the map and kicks the old backend's bots at its start, including
  `addbot` ones.
* `create_bot` is refused unless the caller's backend is active, so a backend sees only its own
  bots.
* Version 7: a backend that registers after the map's selection takes over only from native. If
  another plugin backend runs, the `ts_bot_backend` change reloads the map as usual. The switch
  happens mid-map, so native bots are kicked then rather than at a map start.
* Licence: `ts-plugin-api` is MIT, like the rest of the workspace (`LICENSE`), so GPL bot
  libraries such as tsmod-rcbot-rs build against a permissively licensed interface crate
  (#104 §5).
