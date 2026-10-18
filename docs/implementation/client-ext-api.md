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
| The loader, the approval gate seam, dispatch, the table's logic | `crates/client/src/ext/` (`mod.rs`, `gate.rs`, `events.rs`, `view.rs`, `intercept.rs`; minor 2: `alive.rs`, `takeover.rs`), no `unsafe` |
| The HUD elements extensions hide and locate (minor 2) | `crates/client/src/hud/elements.rs`, the element draws in `hud/` (`mod.rs`, `status.rs`, `weapons.rs`), `menus::paint_vgui` (the scoreboard), `spectator::redraw` |
| The approval store's gate, start-up verification and prompt (#297) | `crates/client/src/plugins/` (client-plugins.md §5, §7.1) |
| Opening a library, `TSRS_ClientExtLoad`, the table's `extern "C"` functions | `crates/client-dll/src/ext.rs` |
| Engine wrappers it added | `goldsrc::ext::gl` (`renderer`, `proc_address`, `with_saved_state`), `ClEngine::server_address` (`goldsrc::ext::hud`); minor 1: the command filter (`goldsrc::client::set_command_filter`) and the server command filter (`goldsrc::ext::hud::set_server_cmd_filter`, `ClEngine::server_cmd_unfiltered`); minor 2: `goldsrc::ext::gles_state` (the GLES save), `goldsrc::ext::hud_record` (draw bounds and dry runs of the 2D HUD primitives), `Cvar::lookup` (#315) |
| `TSExt` (minor 1): the wire format; the server's sender | `crates/shared/src/protocol/ext.rs`; `crates/server/src/plugin_api/ext.rs` (plugin API version 8, plugin-api.md §13) |
| The HUD's state/display split for intercepted messages | `crates/client/src/hud/mod.rs` (`on_server_message`, `handle`, `display_released`) |
| The stub extension and its helper | `crates/tsmod-client-test` (`cargo xtask client-plugins`, with the server stub `crates/tsmod-test` for `TSExt`) |

## 1. Versions

* `VERSION` (1) is the major version. It must match exactly in both directions: the table's
  `ClientExtApi::version` and the extension's `ClientExt::api_version`. Both structs' sizes must
  match too. A break bumps `VERSION`.
* `MINOR` (2) counts compatible additions. Both structs end in reserved, NULL slots
  (`ClientExtApi::reserved`, 32 at minor 0; `ClientExt::reserved`, 16), so their sizes stay fixed
  while a minor version turns slots into named functions and callbacks. An extension checks
  `ClientExtApi::minor` before calling a function added after minor 0. An older client leaves the
  slot NULL (the Rust types are `Option`s for that reason), and never calls a callback it doesn't
  know.
* **Minor 1** (#294, #295) named 2 table slots and 4 descriptor slots (30 and 12 stay reserved):
  `ClientExtApi::claim_channel` and `release`; `ClientExt::user_message`, `message_pre`,
  `command_pre` and `server_cmd_pre` (§10, §11). An extension built for minor 0 loads unchanged.
* **Minor 2** (#306, #314) named 3 table slots and 2 descriptor slots (27 and 10 stay reserved):
  `ClientExtApi::hud_set_hidden`, `hud_elements` and `menu_reply`; `ClientExt::menu_shown` and
  `menu_closed` (§12). It also added `local::DUCKING` in `LocalState::flags`, left dead players
  out of `visible_players` (§6), and saves the GLES state around `redraw` (§4). An extension built
  for minor 1 loads unchanged and works as before (§12: its block of a menu stays a block), and
  one built for minor 2 checks `ClientExtApi::minor` (a minor-1 client leaves the new slots NULL
  and never sets `DUCKING`).

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
2. Adds the approval gate's packaged candidates (`ApprovalGate::packaged`, §3: each installed
   package's library for this platform, `cl_plugins/<id>/<version>/files/...`, named `<id>`).
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

## 3. The approval gate

`ts_client::ext::gate::ApprovalGate` is what the loader and `verified_spawn` ask:

```rust
pub trait ApprovalGate {
    fn packaged(&self, game_dir: &Path) -> Vec<Candidate> { Vec::new() } // cl_plugins/<id>/<version>/ libraries
    fn check_library(&self, c: &Candidate) -> Result<Approved, String>;    // hash everything, compare with the store
    fn check_spawn(&self, plugin: &Approved, library: &Path, exe: &Path) -> Result<(), String>; // re-hash before a spawn
    fn server_changed(&self, server: Option<&str>) -> Vec<Candidate> { Vec::new() } // scope = "server" ones to load now
    fn waits_for_server(&self) -> bool { false }
}
```

`Approved` carries the plugin id (`webview`, or `manual:<name>` for a hand-installed library), the
directory name under `cl_plugins_data/`, and a source for the console.

* **The store's gate** (`ts_client::plugins::StoreGate`, issue #297): `ts-client-dll`'s
  `Initialize` calls `ts_client::plugins::install()`, which verifies every plugin with
  `plugin-pkg`, shows the start-up prompt and installs the gate with `set_gate`, then
  `ext::load_all()`. `packaged` lists the installed packages' libraries; `check_library` maps
  only an allowed, byte-for-byte match whose `[requires] port` and `client_ext_api` this client
  meets, hashing again anything a prompt stood in front of; `check_spawn` re-hashes the file
  against the plugin's approval (`Entry::check_file`; any approved file of the plugin, so for a
  hand-installed library the files its `tsmod_<name>.files` lists, or that list itself). A
  `scope = "server"` approval doesn't load at start: the loader watches the server
  (`waits_for_server`) and loads it through `load_one` on connecting to a listed address
  (`server_changed`), after which it stays loaded (client-plugins.md §9). `cl_plugins 0` gives
  no candidates and denies everything.
* **Without a gate installed:** `DenyUnapproved`. Nothing is approved, so nothing loads
  (client-plugins.md §1: nothing loads without the player's approval).
* **Development override:** in a **debug build only** (`debug_assertions`, the profile
  `cargo xtask install` uses by default), `TSRS_DEV_CLIENT_EXT=name,name,...` in the environment
  approves those hand-installed `cl_dlls/tsmod_<name>` libraries without hashing, and lets
  `verified_spawn` start the files listed (one relative path per line) in
  `cl_dlls/tsmod_<name>.files` (client-plugins.md §5.3). The loader says so on the console. A
  release build ignores the variable and says that instead. It wraps the store's gate
  (`gate::with_dev_override`), and start-up neither verifies nor prompts for the libraries it
  names. `cargo xtask client-plugins` approves its fixtures with `xtask plugin-approve` and keeps
  one (`test_refuse`) on the override, so both stay tested.


## 4. Callbacks

All optional; all on the engine's thread, from inside a client export. Load order. A suspended
extension gets nothing but `resume` and `shutdown`.

| Callback | Called from |
|---|---|
| `init()` | the end of `HUD_Init`: register commands and cvars here, not in the load function |
| `vid_init()` | the end of `HUD_VidInit` |
| `frame(time)` | the end of `HUD_Frame` |
| `redraw(time, intermission)` | `HUD_Redraw`, after the HUD and the menus, with the GL state saved around the extensions (`goldsrc::ext::gl::with_saved_state`). Desktop GL: all attributes, client attributes and the projection, model-view and texture matrices. GLES (minor 2, #306; `goldsrc::ext::gles_state`): no attribute stack, so the specific state read with `glGet*` and set back: blend, depth test, cull, scissor and stencil enables, blend factors and equations, depth mask and function, viewport, scissor box, colour mask, the active texture unit and the 2D texture on it and on unit 0, pack/unpack alignment; on GLES 2+ the program, array and element buffers, framebuffer, attribute arrays 0..8 enabled and (GLES 3 / GL 3 contexts) the vertex array object; on GLES 1 the 2D-texture and alpha-test enables, the current colour, the client arrays and the three matrices (pushed). The generation comes from `GL_VERSION`. Nothing on the software renderer |
| `key_event(down, key, binding) -> swallow` | the start of `HUD_Key_Event`, before the port; a swallowed key reaches neither the port nor the engine. Mouse buttons come as `K_MOUSE1`.. keys |
| `mouse_active(active)` | `IN_ActivateMouse` / `IN_DeactivateMouse` |
| `wants_cursor() -> bool` | once a frame, from `HUD_Frame`. True takes the mouse: the view holds still and mouse motion moves the port's virtual cursor, as for the port's menus (`input::cursor_owned`) |
| `usercmd(cmd)` | the end of `CL_CreateMove`: a copy with `buttons` (TS's bit map, the `button` module), `pressed`/`released` edges against the previous command, view angles, moves, `impulse`, `weaponselect`, `active` |
| `game_event(event)` | after the HUD handled a message (§5) |
| `suspend(reason)` / `resume()` | `ts_client::ext::suspend`/`resume`: a server's `block` with `suspend` (client-plugins.md §13.3, #302; the reason shows in `cl_plugins_list`), the dev aid `ts_dev_extsuspend`; a server change (a new address or a disconnect) resumes every suspended extension, with a console line |
| `server_changed(address)` | `HUD_Frame`, when the net API's server changes: `loopback`, `a.b.c.d:port`, or `""` on disconnect |
| `shutdown()` | `HUD_Shutdown`, even while suspended |
| `user_message(channel, data, len)` (minor 1) | a `TSExt` message on a channel the extension claimed (§10) |
| `message_pre(msg, out, out_len) -> verdict` (minor 1) | each user message from the server, before the HUD (§11) |
| `command_pre(cmd) -> verdict` (minor 1) | each client command the port registered, before it runs (§11) |
| `server_cmd_pre(cmd, out, cap) -> verdict` (minor 1) | each command the port's code sends to the server (§11) |
| `menu_shown(kind) -> bool` (minor 2) | `HUD_Frame`, once for each menu the extension took over and hasn't answered; 0 gives it back (§12) |
| `menu_closed(kind, reason)` (minor 2) | a takeover ended without the extension's answer: `EXPIRED`, `REPLACED`, `RESET`, `GIVEN_BACK` (§12) |

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

Every function is non-NULL in this client (minor 1's and 2's are `Option`s in Rust only because an
older client leaves them NULL). Strings are NUL-terminated; string results go into a caller's buffer
(cut to fit, always terminated) and the call returns the full length. Codes: `OK` 0, `NOT_FOUND`
-1, `INVALID` -2, `UNAVAILABLE` -3, `SUSPENDED` -4, `EXISTS` -6, `REFUSED` -7, `FAILED` -8.

* **Host:** `print`; `add_command` (one port trampoline dispatches by name, so a suspended
  extension's commands do nothing; `EXISTS` for a name another extension or any cvar has, while
  the engine itself refuses its own and the port's command names with a warning), `cmd_argc`,
  `cmd_argv`; `register_cvar` (`EXISTS` for any existing cvar: FWGS would reset it), `cvar_value`,
  `cvar_string`, `set_cvar_value` (`Cvar_SetValue`). These look cvars up with `Cvar::lookup`, a
  walk of the engine's cvar list from `pfnGetFirstCvarPtr`, not `pfnGetCvarPointer`: FWGS's
  `CL_CvarGetPointer` warns "client tried to get non-existent cvar" for every miss, which is the
  normal answer to a clash check (#315). `client_cmd`, `server_cmd` (straight to the
  engine: an extension's own commands to the server, the webview's `wv`, pass no
  `server_cmd_pre`); `screen_size`,
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
  armour, weapon with clip and reserve as the HUD shows them, `ON_GROUND`/`OBSERVER`/`INTERMISSION`/
  `DUCKING` flags, eye, angles, forward, velocity, speed); `player(slot)` and `scores` (scoreboard rows:
  name, team, frags, deaths, ping, local/spectator/same-team flags, no positions); `aim` (the HUD's
  crosshair-ID trace from the eye, 8192 units, players solid: none/world/player/entity, distance,
  point, slot); `trace_line` (world only: players are never solid in it); `visible_players`; `view`
  (screen, FOV, intermission, map); `world_to_screen`; `weapon_name`; `max_clients`.
* **`visible_players`:** a player counts when it isn't the local player, was in this frame's
  packet (`messagenum`), is drawn (no `EF_NODRAW`), isn't an observer, isn't dead, projects inside
  the screen, and a world-only trace from the eye reaches its head (origin + 24) or its centre.
  Nearest first, with bearing (positive left), elevation (positive up), screen position and
  origin. Cached for the frame.
* **Dead** (minor 2, #306; `ts_client::ext::alive`): the client has no other player's health or
  `deadflag` (`delta.lst` sends them in the client data only), so a player is dead from a
  `DeathMsg` naming it the victim (blocked and held ones too: state is applied) until its entity
  comes in a packet newer than the death with a living movetype. A dying player's is
  `MOVETYPE_TOSS` (`CBasePlayer::Killed`), then `MOVETYPE_NONE` once it lands
  (`PlayerDeathThink`, 0x8eaec); `CBasePlayer::Spawn` sets `MOVETYPE_WALK`. The packet check is
  there because a `DeathMsg` is read before its packet's entities. A `Spectator` message for the
  player (it became an observer, or stopped being one) ends it too, and `InitHUD` and a server
  change clear every mark. The body left when the player turns observer is a `deadcorpse` entity
  (`TSCreateDeadCorpse`, 0x70234), outside slots 1..maxClients, so it never counts. Rejected:
  TS's death sequences on the entity, whose numbers depend on each player model.
* **`LocalState::flags`** (`local::*`): `ON_GROUND` (`V_CalcRefdef`'s `onground`), `OBSERVER`,
  `INTERMISSION`, and (minor 2, #306) `DUCKING`: `FL_DUCKING` in `HUD_PostRunCmd`'s predicted
  `to.client.flags`, the newest command's, so set once the duck has finished (the ducked hull),
  as the server's movement sets it; never for an observer or a dead player.

**On-screen only.** Nothing gives the position of a player the screen doesn't show, and the one
trace through players starts at the eye. That is why the webview spec's
`player(slot)` has no origin here and `trace_line` has no `players_solid`.

## 7. Dev aids (port additions)

All need `ts_dev_scriptinput 1` and `sv_cheats 1`, like scripted input:

* `ts_dev_key <key> <1|0>`: the engine's `Key_Event`, as a key would, so scripts reach
  `HUD_Key_Event` (and the extensions' `key_event`).
* `ts_dev_extsuspend <name> <1|0> [reason]`: suspends or resumes an extension.
* `ts_dev_extlist` (no gate): the loaded extensions.
* `ts_dev_extlog 1` (a cvar, no gate): the interception hooks' decisions as `[ts ext]` console
  lines: each presentation message passed, rewritten, or blocked or held (with its printable
  text), each release, each blocked client command, and each command to the server as it was sent
  or blocked (§11); minor 2's menu takeovers and HUD masks (§12).

## 8. Deviations and gaps

* **The client library's own suffix** isn't used: extensions use the server plugins' suffixes (§2).
* **GLES state is the listed state only** (§4): GLES has no attribute stack, so uniforms,
  attribute pointers, texture parameters and other units' or targets' bindings aren't kept, where
  the webview spec says "GL state saved and restored". Under `gl4es` the port reads and sets the
  GLES driver's state beneath gl4es, which may hold state it hasn't sent yet. The software
  renderer has nothing to save.
* **"Alive" in `visible_players` is the client's inference** (§6): TS's client has no per-player
  dead flag and the server sends none; a player that dies and respawns while out of view, without
  becoming an observer, counts as alive from its first packet in view, as it is.
* **Menu takeover is the port's menus** (§12). The webview spec's `reply` also names "a buy
  item", but TS's buy menu isn't a server message (the client opens it on `Spectator`, which is
  observe-only), so it can't be taken over. A taken-over menu's handle expires on its kind's newer
  menu, its `ShowMenu` time, `ResetHUD` and a server change, as the spec says; on fail open the
  port shows the menu from its messages, so a `ShowMenu`'s display time starts again then.
* **Centre prints** (`hud::CENTRE`) are drawn by the engine (`pfnCenterPrint`): while hidden, a new
  one isn't printed at all (it isn't shown later), and `hud_elements` has no rectangle for it. The
  HLTV overview map is drawn in the 3D pass and isn't part of `hud::SPECTATOR`. `hud::PICKUPS`
  exists for the spec's list; TS 3.0 draws no pickup notices.
* **The plugin offer flow** and its cards (#299) and `cl_plugins_menu` (#311) are
  client-plugins.md's.
* **`svc_stufftext` can't be intercepted.** The engine runs a server's stuffed text itself; no
  client library sees it, so there is no hook for it (the webview spec §7.6 lists it as
  unreachable too). `server_cmd_pre` is about the other direction: commands the port sends.
* **`command_pre` only blocks.** The webview spec's `Replace` for commands applies to commands to
  the server (`server_cmd_pre`); a client command's verdict is pass or block, since the port
  can't re-run the engine's tokenised command with other arguments. Its `chat_send` (`say`,
  `say_team`) is the engine's command, not the port's, so it doesn't reach `command_pre` either.
* **No raw `message_begin(TSExt)` guard on the server.** The webview spec §4.2 has the server
  refuse an AMX Mod X `message_begin` of `TSExt` to clients without `_tsrs` (and the port's own
  gate also refuses port clients before 3.0.9, which don't hook it). AMX Mod X writes
  messages straight to the engine, past the game library, so only a metamod hook can (filed as
  tsmod-amxx-rs#22). The plugin API's `ext_message` checks `_tsrs` itself.
* **The port's hold timeout** is 1 s (`HOLD_MS`), a safety net over the webview's own 500 ms cap
  per event.

## 9. Tests

* Unit tests: `cargo test -p ts-client-ext-api` (the sizes are pinned, minor 0's included),
  `-p ts-client ext` (gate, events, edges, bearings, descriptor checks; `ext::intercept`: what each
  message and command allows, replacement checks, order, holds, timeouts, suspension, the queue
  limit), `-p shared plugin_lib`, `-p shared protocol::ext`, `-p ts-server plugin_api::ext`,
  `-p goldsrc net_tests`, `-p xtask client_plugins`.
* **`cargo xtask client-plugins [fwgs fwgs-win ...]`** (dev profile only; FWGS roots, default
  both; roughly a minute per root, longer under Wine): installs the stub as `cl_dlls/tsmod_test`, its
  helper and `tsmod_test.files`, and the fixtures the loader must skip (another API version, a bad
  descriptor size, a refusal, the client library renamed (no export), `tsmod_unapproved` (the
  gate's denial), the other architecture, junk, another platform's name). `xtask plugin-approve`
  approves `test`, `test_badver`, `test_badsize` and `noexport` in a fresh `approved.toml`;
  the game runs with `TSRS_DEV_CLIENT_EXT=test_refuse`, and with the approval fixtures of
  client-plugins.md §11 e2e 1 (packages, tampering, the prompt hook) beside them. The listen-server
  script on ts_dojo joins and spawns, sets `tsext_test_cvar 5`, presses `K_AUX32` with
  `ts_dev_key`, `+use` and `+attack`, adds a stopped bot in front of the host, runs
  `tsext_test_spawn` (a file outside the approval refused, the listed helper started), suspends the
  extension and runs its report command (which must not run), resumes it, runs
  `tsext_test_report`, kills the host, reports again and runs `cl_plugins_list` and
  `cl_plugins_verify`. xtask checks the loader's console lines, the helper's `spawned.txt`, and
  every check of `tsmod_client_test::CHECKS` in the stub's `report.txt` (in its data directory, so
  the Wine log buffer doesn't matter). Fixtures are removed afterwards and by every `install`.
* **Minor 1 in the same run:** xtask also installs the server stub plugin (`crates/tsmod-test`)
  as `dlls/tsmod_test`. With the bot in, the script turns on `ts_dev_extlog`, runs
  `tsext_test_tsext` (the extension's `tsext_echo hello` to the server; the server plugin answers
  on channel `test` with what `ext_message` and `client_port_version` said about the bot, a
  broadcast, a payload too long and a bad channel, and also sends on an unclaimed channel), then
  `special` three times with `tsext_test_arm block` (`command_pre` blocks it: no `_special` may
  follow), `pass` (`server_cmd_pre` sees `_special`) and `rename` (a replacement `kill`, which the
  port refuses), then says `tsext_block_me` (blocked), `tsext_hold_me` (held 20 frames, then
  released; a second release is `NOT_FOUND`) and `tsext_pass_me`. The final `kill`'s `DeathMsg`
  is rewritten (its weapon `tsext_rewritten`) while its `DEATH` event still comes. xtask checks
  the stub's new checks and the `[ts ext]` lines, the kill feed's `with tsext_rewritten`, and the
  replacement's warning.
* **Minor 2 in the same run** (#306, #314, #315): the script holds `+duck` for two seconds (the
  stub's `local_ducking` check), hides the health box and chat with `tsext_test_hud hide` (the
  stub checks that `hud_elements` still places the hidden health box: `HIDDEN`, `DRAWN`, `RECT`)
  and shows them again (`hud_elements` at the report: all 20, named, the health box placed);
  `tsext_test_menu take` and `motd` (the server's `SendMOTDToClient`, with `motdfile` pointed at a
  file xtask writes for the run: `script_start`'s points at none) make a takeover, `bad`
  checks `menu_reply`'s refusals (`INVALID` for a key of a MOTD and an unknown kind, `NOT_FOUND`
  for a kind it didn't take over), `close` answers it (`OK`, then `NOT_FOUND`); another `motd` and
  `tsext_test_menu giveback` (`menu_shown` says 0) gives it back (`menu_closed` `GIVEN_BACK`), and
  a third `motd` is given back by the suspension. After the report, `ts_hurt 2 500` kills the bot
  in view, which must be out of `visible_players` 10 frames later. xtask also checks the `[ts ext]`
  lines (`hud hidden by test: 0x41`, `MOTD taken over by test`, `menu 3 answered: Close`, two
  `menu 3 of test given back to the port`) and that FWGS logged no "non-existent cvar" warning
  for the stub's `tsext_*` names (#315).
* Unit tests for minor 2: `-p ts-client-ext-api` (minor 2's slots come after minor 1's),
  `-p ts-client ext::alive`, `ext::takeover` (what each menu offers, chunks, replies, expiry,
  replacement, give back), `ext::intercept` (menu blocks recorded), `ext::tests` (the hidden mask
  without suspended extensions), `ext::view` (local flags), `hud::elements`, `-p goldsrc
  ext::gles_state` (a fake GL context: GLES 1 and 2+ state set back, the order of the VAO and its
  buffers), `ext::hud_record` (bounds, dry runs, records inside records).

## 10. `TSExt` channels (minor 1, #294)

`TSExt` is a port user message, registered after TS's 64 and before `TSPkg`
(`shared::protocol::Msg::TsExt`, variable size; client-plugins.md §12): `string channel`, then the payload to the end of the
message (`shared::protocol::ext`). The channel is 1 to 15 bytes of ASCII letters, digits, `_`,
`-`, `.`; the whole message is at most 192 bytes (`MESSAGE_MAX`, the Steam engines'
`MAX_USER_MSG_DATA`; FWGS allows 2048), so the payload is at most 192 minus the channel and its
NUL: 184 bytes on `webview`, 176 on a 15-byte channel (the webview spec's figure).

* **Server:** plugin API version 8's `ext_message(player, channel, data, len, flags)`
  ([plugin-api.md](plugin-api.md) §13) sends one message to a player, or with player 0 to every
  port client, `MSG_ONE` each (`ext_flag::UNRELIABLE` for `MSG_ONE_UNRELIABLE`). Only clients
  that hook `TSExt` get it, by the same gate as `TSPkg` (client-plugins.md §12): a `_tsrs`
  version at or above `port::TSPKG_SINCE` (3.0.9), or any readable one with `ts_dev_tspkg 1` and
  `sv_cheats 1` (builds between releases report the last release's version). TS 3.0's client
  and port clients up to 3.0.8 don't hook it. A player whose client doesn't (TS 3.0's client, an
  older port client, a bot, HLTV) is `UNSUPPORTED`; a channel or payload that doesn't fit is
  `INVALID`.
* **No chunking in the port.** One message is one delivery, so a server can also send `TSExt`
  itself (AMX Mod X's `message_begin(MSG_ONE, get_user_msgid("TSExt"))`). A longer body is the
  channel's protocol to split: the webview's `op, msg_id, chunk` header (its spec §8.1).
* **Client:** `claim_channel(ext, name)` gives a channel to an extension (`EXISTS` if another
  has it; claiming one's own again is `OK`). Each `TSExt` goes to its channel's extension's
  `user_message`, unless that extension is suspended. A message on an unclaimed channel, or a
  malformed one, is dropped without a word. `TSExt` never reaches the HUD or `message_pre`.
* **Client to server:** an extension sends with `server_cmd` (`wv ...` for the webview, its spec
  §8.2); a server plugin gets it as a client command (`Plugin::client_command`, or AMX Mod X's
  `register_clcmd`). Nothing new was needed for this direction.

## 11. Interception (minor 1, #295)

Three hooks see what passes between the server, the player and the HUD, in load order. Each call
says what its verdict may do (`allow`: `BLOCK` 1, `REPLACE` 2, `DEFER` 4; 0 is observe-only) and
the port ignores anything else, with a console warning once per extension and case. Verdicts:
`PASS` 0, `BLOCK` 1, `REPLACE` 2, `DEFER` 3. The rules are the webview spec's state/display split
(§7.6, §7.10), enforced by the port (`ts_client::ext::intercept`, pure and unit tested):

| What | `allow` |
|---|---|
| `SayText`, `DeathMsg`, `TextMsg`, `HudText`, `TSMessage`, `Objective`, `ShowObj`, `ShowMenu` | block, replace, defer |
| `TSPAward` | block, defer (the player it names can't change) |
| `WeapPickup`, `AmmoPickup`, `ItemPickup` (TS 3.0 draws no pickup notices), `VGUIMenu`, `MOTD` | block |
| every other message (state: health, weapons, scores, teams, timers, spectator, ...; effects: fades, shakes, blur, damage, slow motion, ...) | observe only |
| `SrvSett`, and every port message after TS's 64 (`TSExt`, `TSPkg`) | never offered |
| the port's client commands (`slot1`..`slot10`, `invnext`, `invprev`, `buy`, `special`, `cancelselect`, `+commandmenu`, `togglebrowser`, ...) | block |
| input buttons (every `+`/`-` command), `ts_dev_*`, `ts_hud_selftest`, `cl_plugins*`, `tsrs_*`, extensions' own commands | observe only |
| commands the port sends the server (`weapon_N`, `drop_N`, `menuselect N`, `jointeam`, buy items, `_special`, `usepowerup`, `lasclr_N`, `respawn`) | block, replace |
| `tspkg` (the plugin offer replies), `vban`, `VModEnable` | observe only |

* **Where:** `message_pre` from the HUD's message hooks (`hud::on_server_message`), before any
  handling; `command_pre` from `goldsrc`'s one command trampoline (every `ClEngine::add_command`
  name; the engine's own commands, `say` among them, never reach it); `server_cmd_pre` from
  `ClEngine::server_cmd`, the one wrapper the port's code sends through.
* **Order:** the first extension that blocks ends the run (later ones aren't asked). A
  replacement is what the next extension sees. A defer pauses the run at that extension until it
  calls `release(token, verdict, data, len)` with pass, block or replace; the run then goes on with
  the next extension, which may defer again (a new token).
* **State and display:** an intercepted message's state is applied when it comes, whatever the
  verdict (`hud::handle` with `Parts::State`): for `DeathMsg` the scoreboard's dead marker
  (`menus::on_message`) and the spectator's death notice; for every message the extensions'
  `game_event`s (so a blocked or held `DeathMsg` still raises `DEATH`). Only the display (the HUD
  element, the console line, the sound) waits for the verdict, and gets the replacement. A message
  nothing changed is handled as before (`Parts::Both`).
* **Arrival order:** a held message holds back the later messages of its kind (not other kinds):
  they run through the hooks when they come, then wait to be displayed. At most 32 wait per kind
  (`HOLD_QUEUE`); past that the oldest is displayed as it is. A hold older than 1 s (`HOLD_MS`)
  passes (fail open), checked each frame.
* **Replacements** must decode as their message, fit in 192 bytes, and keep who the message names
  (`SayText`'s sender, `DeathMsg`'s killer and victim; `TSPAward` isn't replaceable); a server
  command's must keep its name and hold no `;` or line break. Anything else counts as a block,
  with a warning. A `message_pre` that returns `REPLACE` with a length outside its buffer blocks.
* **Suspension** releases the extension's holds as passes and skips it in every run; a server
  change (connect, disconnect) drops whatever is held.
* **What a hook can't do (client-plugins.md §7.3):** approve anything, or stop the approval path.
  `tspkg` replies and the `cl_plugins*` / `tsrs_*` commands are observe-only, a replacement can't
  turn one command into another, the `TSPkg` offers won't reach `message_pre` (no port message
  after TS's 64 does), and the extensions' own `client_cmd`/`server_cmd` were already possible
  without hooks. Keeping the approval cards' input and drawing from extensions (`key_event`,
  `redraw`) is the prompt's part (#297, client-plugins.md §7.3 "No cover").
* **`release` from inside `message_pre`** finds nothing (`NOT_FOUND`): the holds are being
  worked on. Release later (a frame, a command).

## 12. HUD control and menu takeover (minor 2, #314)

The webview spec's §7.8 and §7.9, enforced by the port.

**HUD elements** (`ts_client_ext_api::hud`, 20 ids in the spec's order, `hud::NAMES`):

| Id | What the port draws for it |
|---|---|
| `health` | the status panel's box, stance figure, health number and icon, and the team bar |
| `armour` | the kevlar icon and number |
| `ammo` | `DrawWeaponInfo` (weapon name, fire mode, clip and reserve, attachments) |
| `weapon_select` | `DrawWeaponList` |
| `crosshair` | `DrawCrosshair` or the wire panel, and the crosshair ID name |
| `killfeed` | `CHudDeathNotice` |
| `chat` | `CHudSayText` (also at intermission) |
| `notify` | `DrawTSMessages` (`TSMessage`, the buy menu's notices) |
| `centre` | centre prints (the engine's; see §8) |
| `hudtext` | `CHudMessage` (`HudText`, titles) |
| `timer` | `DrawRoundTimer` |
| `cash` | the free slots and credits boxes |
| `kungfu` | `DrawKungFuLabel` and the kung fu icon |
| `stunt` | the movement icon and the divesteps (drawn from the crosshair's code, but their own element) |
| `powerup` | `DrawPowerup` and the slow-motion icon |
| `objective` | `DrawFlagPanel` and `DrawObjectiveMarkers` |
| `pickups` | nothing (TS 3.0 draws none) |
| `status_icons` | `CHudStatusIcons` |
| `spectator` | the spectator panel and the HLTV name tags |
| `scoreboard` | the scoreboard |

Not elements: the text menu, the buy menu, the train, the status bar, teammate markers, laser
dots and the logo.

* `hud_set_hidden(ext, mask)` sets that extension's mask (`OK`; `INVALID` for a bit past the 20;
  `SUSPENDED`). An element is hidden while any extension that isn't suspended hides it
  (`ext::hidden_mask`): a suspension shows that extension's elements again, a resume hides them
  again.
* **Hidden elements keep their state.** Each element's draw code runs inside
  `hud::elements::draw`; a hidden one runs as a dry run (`goldsrc::ext::hud_record::record`), in
  which the 2D primitives (`SPR_Draw*`, `FillRGBA`/`FillRGBABlend`, `DrawCharacter`,
  `DrawConsoleString`, `CenterPrint`) only count what they would cover. So timers, fades and
  expiries move on exactly as when it's drawn, and showing it shows it as it would be now.
* `hud_elements(out, max)` lists the 20 with flags `HIDDEN`, `DRAWN` (it drew, or would have, at
  the last redraw) and `RECT` (`rect` is the bounds of everything it last drew, kept while it draws
  nothing). Rectangles are worked out only while an extension is loaded; with none and nothing
  hidden, `draw` is a plain call, as TS.

**Menu takeover** (`ts_client::ext::takeover`, pure; kinds `menu::TEXT`, `VGUI`, `MOTD`):

* A `BLOCK` verdict (in `message_pre` or a `release`) on a `ShowMenu`, a `VGUIMenu` the port would
  show (the team menu 2 in teamplay, the briefing 4, the MOTD window 5) or a `MOTD` takes it over.
  The port keeps its messages as the blocking extension saw them (a text menu's and a MOTD's
  chunks, until the last) and what it offers: a text menu's keys, the team menu's teams (1 to the
  `TeamNames` count, at most 4, and 5, auto-assign, as `SlotInput`), or closing only.
* `menu_reply(ext, kind, choice)` answers it once: `"N"` a key the text menu has (the port sends
  `menuselect N`, as `SelectMenuItem`), `"N"` a team the team menu offers (`jointeam N` as a client
  command and the local team number, as `CMenuHandler_StringCommandWatch`), or `""` (or NULL) to
  close it with nothing sent (Esc, cancel, OK). Other choices are `INVALID` and the takeover stays;
  no open takeover of that kind of that extension's (or one whose chunks aren't all in) is
  `NOT_FOUND`. The answer goes through `ClEngine::server_cmd` like the port's own, so other
  extensions' `server_cmd_pre` sees it.
* It ends without an answer (`menu_closed`) on a newer menu of its kind, displayed or taken over
  (`REPLACED`), the server closing it (a `ShowMenu` without keys) or its `ShowMenu` time running
  out (`EXPIRED`), and `ResetHUD` or a server change (`RESET`).
* **Fail open:** when its extension is suspended, or its `menu_shown(kind)` (asked every frame)
  returns 0, the port displays the menu from the kept messages (`hud::display_released`), and
  `menu_closed(kind, GIVEN_BACK)` tells the extension (not when suspended). An extension without
  `menu_shown` (built for minor 1) is never asked, so its block stays a block until a suspension.
* `ts_dev_extlog 1` logs `<message> taken over by <ext>`, `menu <kind> answered: <answer>`,
  `menu <kind> reply "<choice>" refused (<code>)`, `menu <kind> of <ext> closed (<reason>)`,
  `menu <kind> of <ext> given back to the port`, and `hud hidden by <ext>: <mask>`.
