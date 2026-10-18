# Client extension API

Port addition (issue #293), not in TS. The port's client library loads native **client
extensions**, `cl_dlls/tsmod_<name>` libraries, through a versioned C ABI, as the server loads its
`tsmod_*` plugins ([plugin-api.md](plugin-api.md)). The first consumer is the webview overlay
(`the-specialists-mod/tsmod-plugin-webview-rs`, its specification §4.1 and §5). Packages,
downloads and the player's approval are [client-plugins.md](client-plugins.md); this document is
the ABI and the loader underneath them.

| Part | Where |
|---|---|
| The ABI: types, constants, contract (`no_std`) | `crates/client-ext-api` (`ts-client-ext-api`) |
| Naming and header rules (shared with the server loader) | `crates/shared/src/plugin_lib.rs` |
| The loader, the approval gate seam, dispatch, the table's logic | `crates/client/src/ext/` (`mod.rs`, `gate.rs`, `events.rs`, `view.rs`), no `unsafe` |
| Opening a library, `TSRS_ClientExtLoad`, the table's `extern "C"` functions | `crates/client-dll/src/ext.rs` |
| Engine wrappers it added | `goldsrc::ext::gl` (`renderer`, `proc_address`, `with_saved_state`), `ClEngine::server_address` (`goldsrc::ext::hud`) |
| The stub extension and its helper | `crates/tsmod-client-test` (`cargo xtask client-plugins`) |

## 1. Versions

* `VERSION` (1) is the major version. It must match exactly in both directions: the table's
  `ClientExtApi::version` and the extension's `ClientExt::api_version`. Both structs' sizes must
  match too. A break bumps `VERSION`.
* `MINOR` (0) counts compatible additions. Both structs end in reserved, NULL slots
  (`ClientExtApi::reserved`, 32; `ClientExt::reserved`, 16), so their sizes stay fixed while a minor
  version turns slots into named functions and callbacks. An extension checks
  `ClientExtApi::minor` before calling a function added after minor 0. An older client leaves the
  slot NULL, and never calls a callback it doesn't know.
* Planned minors: `user_message` and `claim_channel` for `TSExt` (#294), and the interception
  hooks `message_pre`, `command_pre`, `server_cmd_pre` with `Verdict`/`release`, `hud_set_hidden`
  and `menu_reply` (#295). They fit in the reserved slots without moving anything.

The webview spec says size and version checks are exact, "the same as plugin-api". That holds for
`VERSION`; the reserved slots and `MINOR` are a port addition so that #294 and #295 don't break
extensions built for minor 0.

## 2. Loading

At `Initialize` (after the gameplay rulesets register, before `HUD_Init`) the client:

1. Lists `<gamedir>/cl_dlls/` and keeps the files named `tsmod_<name>` plus the platform suffix:
   `.dll` (win32), `_amd64.dll`, `_i386.so`, `_amd64.so`. These are the server plugins' suffixes,
   not the client library's own names (`client.so` on 32-bit Linux), so one `cl_dlls/` can hold
   every platform's build. A file named for another platform, or whose ELF/PE header says another
   platform or architecture, is skipped with a console line (`shared::plugin_lib`). Files are
   taken in name order.
2. Adds the approval gate's packaged candidates (`ApprovalGate::packaged`, §3; none yet).
3. Asks the **approval gate** about each candidate. Nothing the gate doesn't approve is mapped.
4. Opens each approved library, calls its exported `TSRS_ClientExtLoad(api, api_size, info, out,
   out_size)` once, and checks the result: it must return `OK`, leave `out->size` alone, and set
   `out->api_version` to `VERSION`. Anything else (no export, a refusal, a wrong size or version)
   skips the library with a console line. A library stays mapped for the process once its load
   function has run.

Console lines are `[ts] client extensions: ...`; an extension's own `print`s are
`[tsmod_<name>] ...`. Two candidates with the same name load once (the first).

A plugin approved after start (a server offer, client-plugins.md §9) goes through
`ts_client::ext::load_one`: the same gate check and load, then `init` at once and `vid_init` if a
map is loaded.

## 3. The approval gate (the seam for #297)

`ts_client::ext::gate::ApprovalGate` is what the loader and `verified_spawn` ask:

```rust
pub trait ApprovalGate {
    fn packaged(&self, game_dir: &Path) -> Vec<Candidate> { Vec::new() } // cl_plugins/<id>/<version>/ libraries
    fn check_library(&self, c: &Candidate) -> Result<Approved, String>;    // hash everything, compare with the store
    fn check_spawn(&self, plugin: &Approved, library: &Path, exe: &Path) -> Result<(), String>; // re-hash before a spawn
}
```

`Approved` carries the plugin id (`webview`, or `manual:<name>` for a hand-installed library), the
directory name under `cl_plugins_data/`, and a source for the console.

* **Default:** `DenyUnapproved`. With no approval store built in, nothing is approved, so nothing
  loads (client-plugins.md §1: nothing loads without the player's approval).
* **Development override:** in a **debug build only** (`debug_assertions`, the profile
  `cargo xtask install` uses by default), `TSRS_DEV_CLIENT_EXT=name,name,...` in the environment
  approves those hand-installed `cl_dlls/tsmod_<name>` libraries without hashing, and lets
  `verified_spawn` start the files listed (one relative path per line) in
  `cl_dlls/tsmod_<name>.files` (client-plugins.md §5.3). The loader says so on the console. A
  release build ignores the variable and says that instead. It exists so `cargo xtask
  client-plugins` can load the stub before the approval store does; #297's `xtask plugin-approve`
  replaces it.
* **Plugging in the store (#297):** build a gate over `plugin-pkg`'s store and call
  `ts_client::ext::set_gate(Box::new(gate))` from `ts-client-dll`'s `Initialize` before
  `ext::load_all()`. `packaged` lists the installed packages' libraries for this platform;
  `check_library` hashes every file and compares with `approved.toml` (client-plugins.md §5.2),
  returning the reason for the start-up prompt when it fails; `check_spawn` re-hashes `exe` against
  the plugin's approval. The prompt itself is #297's (it can run inside `check_library`, or the
  gate can collect the failures and #297 can load the newly approved plugins with `load_one`).
  `cl_plugins 0` (client-plugins.md §8) belongs in the gate too: return no candidates and deny
  everything.

## 4. Callbacks

All optional; all on the engine's thread, from inside a client export. Load order. A suspended
extension gets nothing but `resume` and `shutdown`.

| Callback | Called from |
|---|---|
| `init()` | the end of `HUD_Init`: register commands and cvars here, not in the load function |
| `vid_init()` | the end of `HUD_VidInit` |
| `frame(time)` | the end of `HUD_Frame` |
| `redraw(time, intermission)` | `HUD_Redraw`, after the HUD and the menus. On desktop GL the port saves all attributes, client attributes and the projection, model-view and texture matrices around the extensions (`goldsrc::ext::gl::with_saved_state`); not on GLES or the software renderer |
| `key_event(down, key, binding) -> swallow` | the start of `HUD_Key_Event`, before the port; a swallowed key reaches neither the port nor the engine. Mouse buttons come as `K_MOUSE1`.. keys |
| `mouse_active(active)` | `IN_ActivateMouse` / `IN_DeactivateMouse` |
| `wants_cursor() -> bool` | once a frame, from `HUD_Frame`. True takes the mouse: the view holds still and mouse motion moves the port's virtual cursor, as for the port's menus (`input::cursor_owned`) |
| `usercmd(cmd)` | the end of `CL_CreateMove`: a copy with `buttons` (TS's bit map, the `button` module), `pressed`/`released` edges against the previous command, view angles, moves, `impulse`, `weaponselect`, `active` |
| `game_event(event)` | after the HUD handled a message (§5) |
| `suspend(reason)` / `resume()` | `ts_client::ext::suspend`/`resume`: a server's `block` with `suspend` (client-plugins.md §13.3, not built yet), the dev aid `ts_dev_extsuspend`; a server change resumes every suspended extension |
| `server_changed(address)` | `HUD_Frame`, when the net API's server changes: `loopback`, `a.b.c.d:port`, or `""` on disconnect |
| `shutdown()` | `HUD_Shutdown`, even while suspended |

## 5. Game events

`GameEvent { kind, a, b, c, text[32] }`, from `ts_client::ext::events` (pure, unit tested):

| Kind | When | Fields |
|---|---|---|
| `SPAWN` | `ResetHUD` (the server sends it on every spawn) | |
| `DEATH` | every `DeathMsg` (the kill feed) | `a` killer slot, `b` victim slot, `c` 1 if the victim is the local player, `text` the weapon |
| `WEAPON` | the HUD's weapon in hand changed | `a` TS id (0 kung fu), `text` its name |
| `HEALTH` | the HUD's health or armour changed | `a` health, `b` armour |
| `TEAM` | `g_iTeamNumber` changed | `a` team |
| `ROUND` | `RoundTime` | `a` seconds, `b` mode |
| `MAP` | `InitHUD` | `text` the map |

## 6. The table

Every function is non-NULL. Strings are NUL-terminated; string results go into a caller's buffer
(cut to fit, always terminated) and the call returns the full length. Codes: `OK` 0, `NOT_FOUND`
-1, `INVALID` -2, `UNAVAILABLE` -3, `SUSPENDED` -4, `EXISTS` -6, `REFUSED` -7, `FAILED` -8.

* **Host:** `print`; `add_command` (one port trampoline dispatches by name, so a suspended
  extension's commands do nothing; `EXISTS` for a name another extension or any cvar has, while
  the engine itself refuses its own and the port's command names with a warning), `cmd_argc`,
  `cmd_argv`; `register_cvar` (`EXISTS` for any existing cvar: FWGS would reset it), `cvar_value`,
  `cvar_string`, `set_cvar_value` (`Cvar_SetValue`); `client_cmd`, `server_cmd`; `screen_size`,
  `mouse_position` (the virtual cursor while a menu or extension owns it); `gl_proc` and
  `renderer_kind` (GL, GLES or software, from FWGS's `r_refdll_loaded` or Steam's `IsHardware`);
  `game_dir`, `port_version`, `server_address`.
* **`data_dir`:** `<gamedir>/cl_plugins_data/<name>/` with a trailing separator, created on first
  use: the only place an extension should write (its package directory is hashed).
* **`verified_spawn(path, argc, argv)`:** `path` absolute or relative to the game directory. The
  gate's `check_spawn` runs first (`REFUSED` and a console line if it says no). The process starts
  in the data directory with stdin, stdout and stderr closed, is reaped on a thread, and the call
  returns its pid.
* **What the player sees** (`ts_client::ext::view`): `local_state` (index, alive, team, health,
  armour, weapon with clip and reserve as the HUD shows them, `ON_GROUND`/`OBSERVER`/`INTERMISSION`
  flags, eye, angles, forward, velocity, speed); `player(slot)` and `scores` (scoreboard rows:
  name, team, frags, deaths, ping, local/spectator/same-team flags, no positions); `aim` (the HUD's
  crosshair-ID trace from the eye, 8192 units, players solid: none/world/player/entity, distance,
  point, slot); `trace_line` (world only: players are never solid in it); `visible_players`; `view`
  (screen, FOV, intermission, map); `world_to_screen`; `weapon_name`; `max_clients`.
* **`visible_players`:** a player counts when it isn't the local player, was in this frame's
  packet (`messagenum`), is drawn (no `EF_NODRAW`), isn't an observer, projects inside the screen,
  and a world-only trace from the eye reaches its head (origin + 24) or its centre. Nearest first,
  with bearing (positive left), elevation (positive up), screen position and origin. Cached for
  the frame.

**On-screen only.** Nothing gives the position of a player the screen doesn't show, and the one
trace through players starts at the eye. That is why the webview spec's
`player(slot)` has no origin here and `trace_line` has no `players_solid`.

## 7. Dev aids (port additions)

All need `ts_dev_scriptinput 1` and `sv_cheats 1`, like scripted input:

* `ts_dev_key <key> <1|0>`: the engine's `Key_Event`, as a key would, so scripts reach
  `HUD_Key_Event` (and the extensions' `key_event`).
* `ts_dev_extsuspend <name> <1|0> [reason]`: suspends or resumes an extension.
* `ts_dev_extlist` (no gate): the loaded extensions.

## 8. Deviations and gaps

The gaps against the webview spec (corpses, ducking, GLES state) are issue #306.

* **The client library's own suffix** isn't used: extensions use the server plugins' suffixes (§2).
* **GLES and software renderers:** no GL state is saved around `redraw` (§4).
* **`visible_players` counts corpses:** the client doesn't know other players' health (TS's
  client neither), so a dead player's body that is drawn and in view is listed. The webview spec
  says "alive".
* **`LocalState` has no ducking flag** yet (the webview spec lists `ducking`).
* **Not built here:** `TSExt` and `claim_channel`/`user_message` (#294); the interception hooks,
  `hud_set_hidden`, `hud_elements` and `menu_reply` (#295); the approval store, the prompt,
  packaged candidates and `cl_plugins*` (#296, #297); a server's `suspend` (client-plugins.md
  §13.3).

## 9. Tests

* Unit tests: `cargo test -p ts-client-ext-api` (the sizes are pinned), `-p ts-client ext` (gate,
  events, edges, bearings, descriptor checks), `-p shared plugin_lib`, `-p goldsrc net_tests`,
  `-p xtask client_plugins`.
* **`cargo xtask client-plugins [fwgs fwgs-win ...]`** (dev profile only; FWGS roots, default
  both; about 40 s on `fwgs`, 70 s on `fwgs-win`): installs the stub as `cl_dlls/tsmod_test`, its
  helper and `tsmod_test.files`, and the fixtures the loader must skip (another API version, a bad
  descriptor size, a refusal, the client library renamed (no export), `tsmod_unapproved` (the
  gate's denial), the other architecture, junk, another platform's name). The game runs with
  `TSRS_DEV_CLIENT_EXT=test,test_badver,test_badsize,test_refuse,noexport`. The listen-server
  script on ts_dojo joins and spawns, sets `tsext_test_cvar 5`, presses `K_AUX32` with
  `ts_dev_key`, `+use` and `+attack`, adds a stopped bot in front of the host, runs
  `tsext_test_spawn` (an unlisted file refused, the listed helper started), suspends the extension
  and runs its report command (which must not run), resumes it, runs `tsext_test_report`, kills the
  host and reports again. xtask checks the loader's console lines, the helper's `spawned.txt`, and
  every check of `tsmod_client_test::CHECKS` in the stub's `report.txt` (in its data directory, so
  the Wine log buffer doesn't matter). Fixtures are removed afterwards and by every `install`.
