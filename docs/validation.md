# Validation

The port is validated on six engine roots with the local TS 3.0 content. This page covers how
each root was set up, how to run the checks, and the latest results.


The static audit of the gameplay logic against the original TS 3.0 and 2.1 binaries, with its
coverage tables, is in [audit/](audit/README.md).

## 1. Engine roots

| Root | Engine | Build | OS | Target installed |
|---|---|---|---|---|
| `fwgs` | Xash3D FWGS (built from `../xash3d-fwgs`, `--64bits`) | 0.21 (2026) | Linux, native | `x86_64-unknown-linux-gnu` |
| `fwgs-win` | Xash3D FWGS 0.21 win32 (from `../xash3d-ts`) | 0.21 | Windows, Wine | `i686-pc-windows-gnu` |
| `fwgs-win64` | Xash3D FWGS `win32-amd64` (the FWGS `continuous` release) | 0.21 | Windows, Wine | `x86_64-pc-windows-gnu` |
| `rehlds-linux` | ReHLDS built from `../ReHLDS` on HLDS `steam_legacy` | 3.x / HLDS 8684 | Linux | `i686-unknown-linux-gnu` |
| `rehlds-win` | ReHLDS 3.15.0.896 release on HLDS `steam_legacy` | HLDS 8684 | Windows, Wine | `i686-pc-windows-gnu` |
| `hlds25-linux` | Stock Valve HLDS, `public` branch (post-anniversary) | 10211 | Linux | `i686-unknown-linux-gnu` |
| `hlds25-win` | Stock Valve HLDS, `public` branch (post-anniversary) | 10210 | Windows, Wine | `i686-pc-windows-gnu` |

### 1.1 Setting the roots up

`fwgs-win64` is the FWGS `continuous` release's `xash3d-fwgs-win32-amd64.7z` unpacked into
`~/games/xash_win64`, with `valve/` filled from the `fwgs-win` root (`cargo xtask install
fwgs-win64` links the TS content into `ts/`). Before it existed the windows-x86_64 server library
had the wrong name (issue #101).

```sh
# steamcmd (one instance per install: steamcmd remembers -beta per app, see Pitfalls)
mkdir -p ~/games/steamcmd && cd ~/games/steamcmd
curl -sSL https://steamcdn-a.akamaihd.net/client/installer/steamcmd_linux.tar.gz | tar xz
./steamcmd.sh +force_install_dir ~/games/hlds_linux +login anonymous \
    +app_set_config 90 mod valve +app_update 90 -beta steam_legacy validate +quit
# Windows files: a separate steamcmd copy with +@sSteamCmdForcePlatformType windows
# HL25: the same with -beta public, into ~/games/hlds25_linux and ~/games/hlds25_win

# ReHLDS for Linux, from source
cmake -S ../ReHLDS -B ../ReHLDS/build -DCMAKE_BUILD_TYPE=Release -DCMAKE_POLICY_VERSION_MINIMUM=3.5
cmake --build ../ReHLDS/build -j
cp ../ReHLDS/build/rehlds/{engine_i486.so,dedicated/hlds_linux,filesystem/FileSystem_Stdio/filesystem_stdio.so} ~/games/hlds_linux/

# ReHLDS for Windows: the official release binaries
gh release download -R rehlds/ReHLDS -p '*.zip'   # rehlds-bin-3.15.0.896.zip
cp bin/win32/{swds.dll,hlds.exe} ~/games/hlds_win/ && cp bin/win32/filesystem_stdio.dll ~/games/hlds_win/FileSystem_Stdio.dll

# HLDS on Linux needs the Steam client library in the place the Steam API looks
mkdir -p ~/.steam/sdk32 && ln -sf ~/games/hlds_linux/steamclient.so ~/.steam/sdk32/steamclient.so

# Wine prefix for the Windows servers (headless: no Mono/Gecko installer dialogs)
WINEPREFIX=~/games/wineprefix WINEDLLOVERRIDES="mscoree,mshtml=" wineboot --init
```

Packages used on Arch: `rustup cmake lib32-glibc lib32-gcc-libs mingw-w64-gcc wine strace`.

## 2. Running the checks

TS content: xtask's dev and test commands link `TS_DIR`, by default `../ts_custom` (TS 3.0 plus
custom maps such as `MecklenburgV_Rain_Final`, which `meck` needs), found from the checkout's
parent directories so worktrees under `.claude/worktrees` use it too. The stock TS 3.0 content,
`../ts_original` (`TS_ORIGINAL_DIR`), is only packaged:

```sh
cargo xtask package-xash-win   # dist --release (windows-i686), then ~/ts-xash-win-release.tar.gz
```

The archive's top directory `ts-xash-win/` holds the FWGS win32 engine from `../xash3d-ts`
(`XASH_WIN_DIR`; symlinks followed, without its `ts/`, `.git` and `engine.log`), `ts/` = the stock
content (symlinks followed, without `scrshots/`, `xtask_*`, `condump*.txt`, `custom.hpk`,
`voice_ban.dt` and the Linux `dlls/ts_i386.so`/`ts_i686.so`) with `dist/windows-i686/ts` over it,
and a CRLF `README-RUST-PORT.txt` naming the commit. `TS_PACKAGE_OUT` changes the output path;
the system `tar` writes it. The path rules are unit-tested (`xtask/src/package.rs`).

```sh
cargo xtask lint                        # no engine-call macros in game crates; TS 2.1 only in crates/ts21; the 3.0 tests
                                        # without the ts21 feature; clippy -D warnings on every target
cargo xtask roots                       # which roots exist
cargo xtask install all                 # assemble <root>/ts and install libraries
cargo xtask server rehlds-linux --map ts_awaken --bots 4 --seconds 60
cargo xtask all                         # test + play, about 4.5 min; then steam if Steam is running and logged in (§5)
cargo xtask test                        # dedicated server + 2 bots, 15 s, every root in parallel (~25 s)
cargo xtask play                        # gameplay on a listen server, fwgs and fwgs-win in parallel (~4 min, meck and bots included)
cargo xtask meck                        # grenades on MecklenburgV_Rain_Final only (~30 s fwgs, ~1 min fwgs-win)
cargo xtask bots                        # TS's bot AI on ts_dojo only: testpath, four addbot bots, botlearn (~45 s)
cargo xtask ts21                        # TS 2.1 mode on every root's dedicated server, the team modes, the clients (~6 min, §2.y)
cargo xtask version [<root>...]         # the port client's version check at connect, Linux roots one by one (~1.5 min each, §2.z)
cargo xtask relaunch [--xvfb] [fwgs fwgs-win]  # restart and reconnect end to end, one root after the other (~5 min each, §2.r)
cargo xtask tspkg [<root>...]           # server offers of client plugins (TSPkg, tspkg, op 5), the client's offer
                                          # flow (cards, downloads, update, op 5's card) and enforcement (blocks,
                                          # match, unlisted), the plugins panel (revoke, unblock,
                                          # delete), Linux roots one by one (§2.z2)
cargo xtask steam [listen|<root>]       # the Steam HL25 client, scripted; friends status invisible meanwhile (§5)
```

`cargo xtask lint` runs `cargo clippy --workspace --all-targets -- -D warnings` for each of the
four targets, so the Windows-only code (`goldsrc::platform::win`, the WinMM joystick) is linted
too; `xtask` itself is Unix host tooling and is linted on the Linux targets only. Each game
crate's `clippy.toml` lists `goldsrc`'s engine-call macros under `disallowed-macros`, a second
guard next to the source check in `xtask/src/lint.rs`.

The roots run in parallel: each one's output is printed as a block when it finishes. Windows
roots share one Wine prefix, so `wineserver -k` runs once at the end instead of after each root.

A server check passes when all of these hold:

* the server answers a status query (A2S_INFO, or the Xash `info` query on FWGS 0.21) with the expected map,
* at least `--bots` players are connected after `--seconds`,
* the log contains `[ts] GameInit`,
* there is no panic, fatal error or segfault.

A line ending in "(port guard)" (with `developer` on) means the game skipped or changed an engine
call that TS makes and the engine would stop on (`implementation/integration.md` §6,
`stock-entities.md` §7.8). It isn't a failure, but on the stock maps none is expected apart from
a respawned `cycler_weapon`'s empty precache on the Steam engines. The guards' decisions are
unit-tested in `goldsrc::guard` and `entities` (`cargo test`). The client prints its own
"(port guard)" lines to the console, once per kind (`implementation/integration.md` §6.1; tests in
`goldsrc::cl_guard`).

Client checks run the FWGS client with an autotest cfg (`wait` chains, commands, `screenshot`,
`quit`) and inspect the screenshots. FWGS joins GoldSrc-protocol servers with
`connect host:port gs`. That covers the full chain: the Rust client on FWGS, connected to the
Rust server on ReHLDS or HLDS.

The gameplay check (`cargo xtask play [<fwgs-root>...]`) is one listen-server run on `ts_dojo`,
on the native and the win32 FWGS builds at once. It plays these scenes in order and reads the
results from `ts_status` dumps, kill notices and developer log lines:

| Scene | How | Passes when |
|---|---|---|
| Join | The buy menu opens with a random first loadout: reset it, pick an AK47 with a scope and confirm (`slotN`, as the number keys do), twice, each time after `buy` (a window resize closes the menu, below), click `+attack` after the respawn wait | `[ts] <host> spawned (#1) with [13]` |
| Wall | `ts_dev_fxlog 1`, stand in front of the dojo's front wall (`100 -760 60`, yaw 70) and fire three single shots | From the `[ts fx]` lines: every first-person flash is more ahead of the eye than off the view axis, within 8 units of the view model's muzzle, and spreads along the view (direction dot > 0.95); three bullets start within 64 units of the shooter and hit within 400; at least three impacts and three decals, all on the wall in front |
| Bot | `ts_bot_stop 1` (the bot stands still and holds fire), `ts_bot_quota 1` (`addbot` bots get `bots.txt` names, so the scenes use a quota bot; it runs TS's AI like an `addbot` bot); while it waits to spawn, `ts_as 2 rst_cfg` and `ts_as 2 tkw 2_2 2_0` make its loadout akimbo Berettas with a laser | The quota reads 1 and a bot was added |
| Lasers | The host gets akimbo Berettas with a laser (`ts_give 2_2` twice; the bot spawned with them), the host switches to free semi and looks at the bot from the side, `ts_dev_fxlog 1` | From the `[ts fx] laser` lines: two first-person beams, each starting at its gun's muzzle in front of the eye (the right gun right of the left one); the bot's beam starts 16 units along it from its `p_` model's gun hand, which is 4–64 units from its origin (attachments from the drawn model, not the origin FWGS resets them to) |
| Akimbo | Free semi: `+attack` and `+attack2` in one frame, then 0.3 s apart | From the `[ts fx] akimbo shot` lines: a linked double shot, then the `b0` gun, then the `b1` gun |
| Power-up | Stand on the slow-motion `ts_powerup` (`0 -672 48`, pwuptype 1) with the bot in view, double-tap `+use` | Held `0x1` after the pickup. Then active `0x1`, host time scale 0.6, bot 0.35 |
| Grenade | `ts_give 24`, `weapon_24`, aim at the 1-HP pillar `*15`, throw (`+attack`, release), move away | `func_breakable ... broken by <host> (grenade, ...)`: the explosion (bits `0x1040`) or the grenade still in flight (`0x1080`), which credits the thrower too (issue #1) |
| Kill | Put the bot in front of the host, aim and fire the AK47 | `<host> killed Bot ...` and a frag |
| Kick | `kick` the bot the host just killed | No crash (a dropped bot used to crash the engine on its "respawn"), and the quota adds a replacement |
| Death and respawn | `ts_hurt 1 500`, click through the death cam, click after the wait | Dead with a death, then a second spawn line and alive |
| Bots | Host suicides and stays observer; `ts_bot_stop 0`, `ts_bot_quota 4`, 70 s of play (TS's AI: the shipped ts_dojo node graph, TS's random loadouts) | Four bots. Within one life, all but at most one of the bots measured (at least two) move over 64 units between dumps 5 s apart. At least 2 `Bot ... killed Bot ...` lines |
| Gameplay reload | With the four bots playing: `ts_gameplay 21;ts_gameplay 30` in one line and `ts_bot_backend NATIVE`, 1.5 s; `ts_gameplay 21` (no `changelevel`), 15 s; `ts_gameplay 30`, then the host joins through the buy menu as in Join (`implementation/gameplay-plugins.md` §4) | Set and back: no `reloading` line, every living player still moves by `gp=30` (`ts_status`) and the scores are kept. The 2.1 map: one `[ts] gameplay: ruleset 21: reloading ts_dojo`, `[ts] gameplay: ruleset 21 for this map`, 2.1's `[ts] game rules (2.1): ...`, the client's `[ts] gameplay: the server runs ruleset 21` (`SrvSett` 4), at least three bots alive and every living player at `gp=21`. The 3.0 map: the same for 30 (one `ruleset 30: reloading ts_dojo`, 3.0's `[ts] game rules: ...`), and the host alive after joining |
| Quota down | `ts_bot_quota 0` | The bots are kicked |
| addbot | `addbot` on the listen server | "<name> connected with skill", a player with a `bots.txt` name in `ts_status`, no "Bots only in listen servers.", and the quota (0) doesn't kick it |
| Escape | `ts_dev_keylog 1`, `cancelselect` twice (Escape's binding; the first may close the weapon menu) | `[ts key] cancelselect: escape`: the client sent `escape`, which opens the engine's main menu |

Screenshots go to `target/play-<root>-N.png` (`-2` is the wall scene). The script can't close the
MOTD, which needs a real Enter key or click, so the scripts set `motdfile` to a file that doesn't
exist and the server sends no MOTD.

The wall scene catches the bug where the client played its predicted weapon events with a zero
origin and zero angles: the shooter's own bullets then flew from the world origin along +x (the
impacts and decals landed there, out of sight) and the first-person flash streaked along world +x
whatever the view (`implementation/client-rendering.md`, "Event parameters"). With that bug put
back, the scene reports `direction-dot 0.342` (cos 70°) and bullets from `[0, 0, 28]`, and all
three wall checks fail.

After that, `play` runs the grenade check on a large third-party map (`cargo xtask meck` runs it on
its own): a listen server on `MecklenburgV_Rain_Final` (37 `func_breakable`s, 158 doors, rain
emitters, fog and wind). Screenshots go to `target/meck-<root>-N.png`.

| Scene | How | Passes when |
|---|---|---|
| Join and bot | As above; one stopped bot | The host spawned alive and a bot was added |
| Grenade 1 | `ts_give 24`, `weapon_24`, stand between the two 10-HP walls `*113`/`*114`, look straight down (pitch 89) and throw, so the M61 drops at the host's feet; teleport away and move the bot onto the grenade | At least one `func_breakable ... broken by <host> (grenade, ...)` before the second throw. A player takes half of the M61's 110, so the bot is left at 45 HP |
| Grenade 2 | The same between the two 50-HP walls `*217`/`*220` | More walls broken (at least three in all), `<host> killed Bot ... with M61 Grenade` and a frag |
| Safety | The host waits at a far spawn point while each grenade goes off | The host is alive at the end; no "(port guard)" line, panic or crash |

The map's doors exposed a bug in the `SOLID_BSP` guard (`stock-entities.md` §7.8): brush
entities link before `SET_MODEL`, and the guard had turned every door into `SOLID_BBOX`. The
"no port guard" check catches a regression.

Then `play` runs the TS bot check (`cargo xtask bots` runs it on its own): TS's bot AI (`mp.dll`
only; `implementation/rules.md` §10) on its own ts_dojo listen server. It needs a server of its
own: the play scene's quota kicks wait behind its script (below), and `maxplayers 8` then leaves
no room for four more bots. The host joins, spawns and suicides, so it watches as an observer.
Screenshot in `target/bots-<root>-1.png`.

| Scene | How | Passes when |
|---|---|---|
| testpath | `botlearn 2` (TS prints `FindPath`'s result only then), `testpath 0 50`, `testpath 10 100`, `testpath 89 25` on the shipped ts_dojo graph | `path:0,47,50`, `path:10,15,63,84,100` and `path:89,88,25`, which is what the original TS 3.0 prints (checked under FWGS win32 with TS's own `mp.dll`; `scripts/ndm.py` gives the first two, see `implementation/rules.md` §10.2) |
| TS bots | Four `addbot`s (TS's `BotCreate`: `bots.txt` names, random models and skills, TS's random loadouts), 40 s of play with `botlearn 2`, `ts_status` every 5 s | Four bots. At least two `(debug)arrived` lines: TS's print for each node path a bot plans to a roam goal, so the bots navigate over the graph. All but at most one of the bots measured move over 64 units within a life, and at least one kill between the bots |
| botlearn | `resetnodes`, 6 s with `botlearn 2`, then `loadnodes` and `botlearn 0` | `(debug)1 nodes`, `(debug)2 nodes`, `(debug)3 nodes`: the bots' walk builds a new graph (each new node prints the count). `savenodes` isn't run: the roots link the content's `maps` directory |

The `path:` replies can show in the client's console after the bots have joined, so the check
searches the whole scene for them.

How the scripts run:
* Game time runs at 2x (`sys_timescale 2`, with `fps_max 100`), and waits are written in game
  seconds (`secs()` in xtask).
* Waits use the `w10`/`w100` aliases. Plain `wait` chains overflow FWGS's 32 KB command buffer
  (`Cbuf_InsertTextToBuffer: overflow`), and the engine then skips the whole cfg.
* The script ends with `ts_dev_touch xtask_<tag>_done`, a marker file xtask polls for. The engine
  log can sit in a write buffer until exit under Wine, so waiting for a line in it used to hold
  the Wine run until its time limit.
* Scripts don't `quit`. Server commands the game queues (the quota's `kick`) wait behind the
  script in the console buffer, so xtask stops the game a few seconds after the marker instead.
* Scripted `+attack` gets past the client's anti-script gate through `ts_dev_scriptinput 1`
  (with `sv_cheats 1`).

Port-only dev aids used by the tests (all need `sv_cheats 1` except `ts_status`):

* `ts_tp <player#> <x> <y> <z> [yaw] [pitch]` teleports a player, drops them to the floor and
  can set their view.
* `ts_face <player#> <target#>` or `ts_face <player#> <x> <y> <z>` aims a player's view.
* `ts_bot_stop 1` makes living bots stand still and hold fire (their AI is skipped). Dead and
  observing bots still buy their loadout and respawn.
* `ts_dev_touch <file>` creates an empty file in the game directory (the end-of-script marker).
* `ts_dev_hamlog 1|0` installs the server's own entity hook and prints a `[ts ham]` line before and
  after every Spawn, TakeDamage and Killed (`implementation/plugin-api.md` §7.6). Refused while a
  plugin's hook is installed.
* `ts_observe <player#> <mode> [target#]` puts an observer in a spectator mode (5 is in-eye) on a
  target, for clients whose input a script can't drive (the original TS client, below).
* `ts_dev_fxlog 1` (client) prints a `[ts fx]` line for each predicted shot's flash, bullet,
  impact and decal, each akimbo shot (`akimbo shot #<ent> weapon <id> linked|gun b0|gun b1`), and,
  at most twice a second, the laser beams (`laser local gun <n> beam ... muzzle ... ahead/right/up`
  for the first-person ones, `laser other #<ent> beam ... hand ... origin ... eye ... aim ...` for
  other players').
* `ts_dev_keylog 1` (client) prints `[ts key]` lines: `HUD_Key_Event`'s answer for Escape
  (`-> 1` hands it to the engine) and what `cancelselect` did.
* `ts_as <player#> <command> [args...]` runs a client command as that player (a bot), e.g.
  `ts_as 2 ts_give 2_2`.
* `ts_status`, `ts_hurt`, `ts_givepowerup`, `ts_give` (existing).
* With `developer` on, each broken `func_breakable` is logged with its attacker, inflictor class
  and damage bits.

### 2.x Plugins

`cargo xtask plugins [roots...]`: a dedicated server per root with the stub plugin `tsmod_test`
and the loader's fixtures; see implementation/bot-plugins.md §6 for what it checks.

`cargo xtask client-plugins [fwgs fwgs-win ...]` (issues #293, #294, #295, #297, dev profile
only): a listen server per FWGS root with the stub client extension `tsmod_test` in `cl_dlls/`, the
loader's fixtures, approval packages in `cl_plugins/` and a fresh `approved.toml` written with
`plugin-approve` (the root's own store is set aside and put back), and the server stub plugin in
`dlls/` for `TSExt` both ways; see implementation/client-ext-api.md §9 and client-plugins.md §11
for what it checks. The `TSExt` and interception checks read `[ts ext]` lines (`ts_dev_extlog`)
from the engine log after the game exits, so they hold under Wine's buffered log too. Like every
client check it needs a display: from a session without one, start an Xvfb and run xtask with
`DISPLAY` set to it, `SDL_VIDEODRIVER=x11` and `WAYLAND_DISPLAY` unset (§4). It passed on `fwgs`
and `fwgs-win` (roughly a minute each, longer under Wine) on 2026-10-18, with both the approval and
the minor-1 checks. The minor-2 checks (#306, #314: ducking, a dying player out of
`visible_players`, the HUD hidden and placed, the MOTD taken over, answered and given back) and
#315's "no non-existent cvar warning" passed on `fwgs` and `fwgs-win` on 2026-10-18 too. Their
script points `motdfile` at a file xtask writes for the run (`script_start`'s `motdfile` names
none, so `motd` sends nothing), and kills the bot with `ts_hurt` (`ts_as <n> kill` does nothing:
`kill` is the engine's `ClientKill` export, not a client command).
The GLES state save (#306) isn't run by any check: xtask picks no renderer, and FWGS's desktop
default is `gl`, so it is covered by the unit tests only (`-p goldsrc ext::gles_state`); a run
with `-ref gles2` (or `gles1`, `gl4es`) would exercise it.

The start-up box itself is answered by the debug-only hook in those runs. The real
`SDL_ShowMessageBox` was checked by hand on `fwgs` under Xvfb on 2026-10-18 (no hook, one new
library): the box showed its text and "Allow", "Not now", "Never" over the windowed game. That
SDL2 also started a zenity box behind it; the game waits in `Initialize` until it's answered, and
ignores SIGTERM meanwhile (`pkill -9 -x xash3d` ends it).

### 2.y TS 2.1 mode (`cargo xtask ts21`)

`cargo xtask ts21 [--no-clients] [roots...]` (issue #103, `xtask/src/ts21.rs`): TS 2.1 mode on a
dedicated server per root, all roots in parallel, then the clients. Each server starts on
`ts_dojo` with four quota bots and `+servercfgfile xtask_ts21.cfg`, which runs the root's
`server.cfg`, sets `ts_gameplay 21`, `developer 1`, `sv_cheats 1` and an `rcon_password`, and
names a second config for every later map (so the rcon changes below stick). xtask then drives
the server by **rcon** (`a2s::rcon`: FWGS takes `rcon <password> <command>`, the GoldSrc engines
want `challenge rcon` first) and reads `ts_status` replies; map changes go through `exec` of a one-line config, so the map load prints to
the console log, not into the rcon reply. From scene 3 on the bots stand still (`ts_bot_stop 1`),
so only xtask's `ts_hurt`s (repeated while a respawned player's spawn protection lasts) end lives.

With `developer` on the server prints what TS shows only on the HUD: every `TSHudMessageAll` text
as `[ts] hud: ...` (`rules::msg::ts_message_all`), 2.1's `TeamWin` and `RestartRound` as
`[ts] round (2.1): ...` with the team cash, scores, mode, side swap and respawns per round, and the
round manager as `[ts] objective: ...` (its pick and each target it fires). The rcon replies are
saved to `target/ts21-<root>.txt`.

| Scene | How | Passes when |
|---|---|---|
| Server config | The first map, `ts_gameplay 21` from the server config | The Steam engines: `ruleset 30 for this map`, then exactly one `ruleset 21: reloading ts_dojo` (they run the config once the map has loaded); FWGS: the first map is already `ruleset 21` (it runs the config before the first map too). Four bots, every player at `gp=21`, 2.1's `[ts] game rules (2.1): DeathMatch`; all but at most one of the living bots move over 64 units in 6 s |
| rcon reload | rcon `ts_gameplay 30`, then `ts_gameplay 21` | Each reloads ts_dojo once (every player at `gp=30`, 3.0's `[ts] game rules:`, then `gp=21`), and no other reload follows: the reload lines total one for 30 and one (FWGS) or two (Steam) for 21 |
| LMS | `respawnnumber 0`, `lastmanstanding 1` on ts_dojo (DM); once everyone is up, all but one player killed | `lms=true`, the survivor gets 10 frags, `[ts] hud: ... is the Last Man Standing! 10 frags` |
| The One | `mp_teamlist "gordon;seal"` (2.1 puts an unlisted team on the smallest listed one, §4), `mp_theonemode 1`, `changelevel ts_dojo`; The One killed | `[ts] game rules (2.1): The One (gordon;seal)`, `... is The One!`, one player on `gordon` and the rest on `seal`; after the kill a One is picked again |
| Model teamplay | `mp_teamlist "hitman;gordon"`, `mp_teamplay 1`, `changelevel ts_dojo`; a player killed | `Team DeathMatch (hitman;gordon)`, every player on `hitman` or `gordon`, the killed player respawns |
| Round TDM | `changelevel cs_krustykrab` (a CS map: `info_player_start`/`deathmatch` only), `respawnnumber 2`; a Specialist killed twice; then `respawnnumber 1` and the Mercenaries killed, three rounds | `Round Team DeathMatch (Specialists;Mercenary)`, everyone on the two teams, `Defeat the opposite team`; the Specialist respawns once and then sits the round out (2.1 counts lives: `implementation/ts21.md`); three `Specialists Win` with `TeamWin(0, 1500, -1000)` and the Specialists' team cash 4500, 6000, 7500 (`cashforwin`), the Mercenaries' at the 3000 floor; the fourth `RestartRound` swaps the sides |
| Objective rounds | `roundtime 30`, `changelevel tm_avana` (`ts_objective_manager`, two `ts_hack`s, three `ts_wingiver`s, `ts_mapglobals playerrespawn 2`); a Specialist killed twice, then every Specialist out; the next round left to run out | `RestartRound: Objective, 30 s, ..., respawns per round 2` (the install line says Round TDM, as 2.1: the managers don't exist yet), the manager picked and `Specialists must hack the computers!`; the Specialist respawns once then sits out; `fires "mercwin" (SpecDead)` and `TeamWin(1, 1500, -1000)` with the Mercenaries at 4500; then `fires "drawwin" (TimeLeft)` and a `TeamWin(2, ...)` draw |
| Large map | `developer 2`, `changelevel MecklenburgV_Rain_Final` | `Can't init <class> (not in this ruleset)` once for `ts_wind`, `ts_fog`, `ts_emitter` and twice for `ts_particle`, `ts_groundammo`; every bot spawns at `gp=21` |

No panic, fatal error or segfault on any map.

Then, on each Linux Steam root among the selected ones (`rehlds-linux`, `hlds25-linux`), a fresh
2.1 server and two clients in turn:

* **The port's client** (FWGS, the `fwgs` root, `connect ... gs`): it connects, the server doesn't
  refuse it, and its log has `[ts] gameplay: the server runs ruleset 21` (`SrvSett` 4).
* **The original TS 3.0 client** (`~/games/xash_win_orig`, `TS_ORIG_CLIENT`: FWGS 0.21 win32 with
  TS's own `client.dll`, under Wine, `connect ... gs`; skipped when missing): the server logs
  `[ts] gameplay: refused a client in ruleset 21 (Other)` and the client's `engine.log` holds 2.1's
  reason. Its own `engine.log` is put back afterwards; the client opens a window for about 20 s.

`--no-clients` skips them. The whole run takes about 6 minutes (the servers about 4, the slowest
being the Wine roots; the clients about 2), which would more than double `cargo xtask all`, so
`all` doesn't include it: run it after changes to the 2.1 plugin, the reload, the rules' round
code or the client check.

### 2.z Client versions (`cargo xtask version`)

`cargo xtask version [roots...]` (issue #300, `xtask/src/version.rs`,
`implementation/client-plugins.md` §13.1): on each Linux root (`fwgs`, `rehlds-linux`,
`hlds25-linux`) the dedicated server runs TS 3.0 mode three times, one client each, with the
port's FWGS client (`fwgs` root; `connect ... gs` for the Steam engines) and the original client
as in §2.y:

1. `sv_client_version ">=99.0.0"`: the server logs `[ts] version: refused` with `[tsrs:>=99.0.0]`,
   the client's log shows that tag, and nobody enters the game.
2. `sv_client_version exact`, `sv_client_require_port 1`: the port's client joins, the server
   refuses nobody, and the client prints no `[ts] version: this server runs ...` line.
3. The same settings (Steam engines only, when `~/games/xash_win_orig` exists): the original client
   is refused, `(Other)` in the server's line and `[tsrs:=<version>]` in its `engine.log`.

Client logs go to `target/version-client-<root>-<case>.log` and
`target/version-orig-client-<root>.log`. Not in `all`: run it after changes to the connect path,
`_tsrs` or the `SrvSett` version keys. The Steam client is covered by `xtask steam <root>`
(§5): its dedicated servers run `sv_client_version exact` and `sv_client_require_port 1`, and a
check fails if the server logs `[ts] version: refused`.

### 2.r Restart and reconnect (`cargo xtask relaunch`)

`cargo xtask relaunch [--release] [--xvfb] [--only <scenario>,...] [fwgs] [fwgs-win]` (issue #301,
`xtask/src/relaunch.rs`, `implementation/client-plugins.md` §11 e2e 5 and 6, §13.5). Two dedicated
servers run on the `fwgs` root (TS 3.0 mode, `ts_dojo`): an open one and one with
`sv_password`. Each client root (default `fwgs`, then `fwgs-win` under Wine) is installed with the
**e2e client** (`--features ts-client-dll/e2e`: cards answered from the file in
`TSRS_E2E_CARDS`, self-relaunch forced on or off by `TSRS_E2E_SELF_RELAUNCH=1` or `0`; neither is a console
command) and `ts-launcher-fake` (`crates/fake-launcher`, copied to `<root>/xtask_launcher/`). The
normal client is installed again afterwards. Scenarios, each starting a client that joins and
runs `ts_dev_restart`:

| Scenario | Set up | Passes when |
|---|---|---|
| `ipc` | `TSMOD_LAUNCHER*`, the fake launcher serving the IPC endpoint | the request arrives, the first game exits, the fake launcher restarts it with `+connect`, it joins |
| `ipc-silent` | the endpoint never answers; passworded server | after 2 s the client starts `ts-launcher-fake relaunch ... --password-required`; the restarted game shows the password card and joins |
| `relaunch-args` | `ts/launcher.toml` only; passworded | `relaunch --instance e2e --wait-pid --connect ... --password-required`, no IPC; card; joins |
| `self` | no launcher, `TSRS_E2E_SELF_RELAUNCH=1` | the engine re-executes itself with `+connect`; joins; no launcher involved |
| `inform` | no launcher, `TSRS_E2E_SELF_RELAUNCH=0`; passworded | the inform card; "Quit now" quits; `.reconnect` has the address and `password_required = true`; nobody rejoins |
| next start | `.reconnect` from `inform` | "Reconnect to ..." with the password field; cancelled, nothing connects and the file is gone; then a fresh file and a click joins |

Every scenario also fails if the password (`e2e-secret-4711`) appears in the fake launcher's log
(every IPC line, every argument, its whole environment) or in `.reconnect`. A join is read from
the server, so no client log is needed: rcon `status` must list a spawned `xtask_e2e` (a ping, not
`Connect`/`Spawning`/`Zombie`) heard from in the last 3 s. The A2S player count isn't enough: it
counts a client from its first `connect` packet, and a killed game until `sv_timeout` drops it.
Each scenario first waits for the server to be empty (A2S), which the "nothing connected" checks
use too. The logs are in `target/relaunch/` (`*-game.log` the first game, `*-launcher.log` what
the fake launcher got, `*-cards.txt.shown` the cards, and under Wine `*-engine.log`, the last
`engine.log` of the scenario). `--only ipc,self,...` runs some scenarios (`inform` includes the
next-start ones).

Test-server settings and why (issue #332): `sv_timeout 20` and `sv_connect_timeout 30` shorten
the wait for a killed game to leave, and `sv_connect_timeout_ban 0` is needed because FWGS
otherwise bans an address whose connecting client times out (`addip 2`). A game killed
mid-connect would lock 127.0.0.1 out of the next scenarios. The run kills its Linux games with
SIGKILL: SDL turns SIGTERM into `SDL_QUIT`, which FWGS passes to the menu's quit question, so the
game stays connected.

`--xvfb` runs the games on `Xvfb :87` from `~/scratch/xvfb` (or `XVFB=<path>`), so nothing opens
on the desktop. The run kills the games it started (Linux: every process running the root's
`xash3d`, also at the start of each root's run; Wine: `wineserver -k` on the shared prefix), so
run it alone, not next to another xtask run on the same roots. About 5 minutes per root; not in
`all`.

**Runs, 2026-10-19** (`cargo xtask relaunch --xvfb fwgs fwgs-win`, dev profile, FWGS Linux
amd64 and FWGS 0.21 win32 under Wine). The first run, on main, failed on both roots. All of it
was the test (#332): leftover Linux games that SIGTERM didn't stop, joins read from the A2S count,
and FWGS's connect-timeout ban. With those fixed, every scenario passed on both roots in three
consecutive runs. The first ran before the list below changed and the other two after. The `self`
scenario forces the self-relaunch on either way.

**Self-relaunch is on only where the `self` scenario passed** (`SELF_RELAUNCH_VERIFIED` in
`crates/client/src/relaunch/plan.rs`). Today that is `FwgsLinux` (`fwgs`) and `FwgsWindows`
(`fwgs-win`). `FwgsWindows` was verified under Wine only. The client can't tell Wine from
native Windows (`Platform` has one FWGS Windows value), so native Windows FWGS gets the
self-relaunch on the strength of the Wine run and is unverified there. The Steam platforms stay
off until #307 has a test. Remove a platform in the same change that records a failing `self`
run here. Not covered yet: Steam HL25 (`steam://run`, #307), the real tsmod-launcher with port
update and downgrade (#308), native Windows, and the card windows themselves, which the e2e hook
answers without opening (#309).
### 2.z2 Plugin offers (`cargo xtask tspkg`)

`cargo xtask tspkg [roots...]` (issue #298, `xtask/src/tspkg.rs`, `implementation/client-plugins.md`
§6, §12, §13.1): on each Linux root the dedicated server runs TS 3.0 mode with a signed stub
package `xtask_stub-0.1.0.tspkg` written to `<root>/ts/plugins/` (with its siblings, by the same
code as `cargo xtask plugin-offer`), a `plugin_offers.toml` offering it as required, and a second
offer of the same file under another id with a wrong hash. The port's FWGS client joins:

1. With `sv_cheats 1`, `ts_dev_tspkg 1` (a build between releases reports the last release, below
   `TSPKG_SINCE`) and `sv_plugin_required_kick 1`: the server logs `offering xtask_stub` and
   `not offering xtask_bad`, sends the offer, and the client prints it with the URL built from
   `sv_downloadurl`. The stub server plugin `tsmod_test` is installed for this part (#321): the
   client script sends `cmd tsmod_test_offer xtask_stub`, and the stub offers through the plugin
   API's `offer_client_plugin` (`offer_client_plugin #1 xtask_stub: 1 unknown=-2`). Then `cmd tspkg
   xtask_stub 0.1.0 offered`, a bare `cmd tspkg list` (the list complete, if the client's own hadn't
   completed it) and `denied`; the server logs them and kicks with "requires the client plugin
   xtask stub 0.1.0 (<homepage>)", and the stub logs `client_plugin_state` for `offered` and for
   `denied` (`loaded=-`: `client_plugin_version` knows of no loaded version) and
   `client_plugin_list` with `client_plugin_version` agreeing with every entry.
2. `sv_client_version ">=99.0.0"`, `sv_plugin_check_time 3`: the client is let in
   (`[ts] version: let in`), gets op 5 and prints the version the server needs, and is kicked
   with the `[tsrs:>=99.0.0]` tag.
3. Without `ts_dev_tspkg`: the offer is still checked, but the client (below `TSPKG_SINCE`, like
   an older port client) gets nothing, and joins. On the Steam engines, when
   `~/games/xash_win_orig` exists, the original client gets no offer and joins. Both run with
   `[policy] unlisted = "kick"` and a `[[block]] id = "manual:*"` kick block added (#302), and
   neither is kicked: nothing is enforced on clients that don't hook `TSPkg`.

The client's side (issue #299, `xtask/src/tspkg_client.rs`, client-plugins.md §11 e2e 2–4): the
`fwgs` root gets the e2e client for the whole run (`--features e2e`, as `cargo xtask relaunch`
builds it, with `ts-launcher-fake` in `<fwgs>/xtask_launcher/`; the normal client is put back at
the end), its approval store is set aside, and xtask serves `<root>/ts` on `127.0.0.1:27999/ts`
(the servers' `sv_downloadurl`) as a FastDL stand-in that can serve other bytes than the server
checked. `TSRS_E2E_CARDS` answers the in-game cards and records them in `<file>.shown`
(`target/tspkg-client/<root>-cards.txt`):

4. Four offers of quiet stub extensions (crates/tsmod-client-test) on port + 4: `quietoffer`
   0.1.0, answered "Allow on this server only", must answer `offered`, `downloading`, `loaded`, be
   installed under `cl_plugins/quietoffer/0.1.0/`, approved for that address only with every
   file's hash, and load late (`quiet stub init`); `quiettamper` (FastDL changes a byte) fails on
   the SHA-256 after its card; `quietbadsig` (FastDL breaks the `.manifest.sig`) and
   `quietunsigned` (unsigned over plain HTTP) fail before any card, and their packages are never
   requested; nothing of them is left in `cl_plugins/`. The client's `tspkg list` reaches the
   server (`plugins: none`).
5. The same server (same port, so 0.1.0 loads on connecting) offers 0.2.0: the update card
   ("0.1.0 -> 0.2.0") is answered "Install update", the client answers `restart` (0.1.0 is
   loaded), and the "Restart now" card's answer makes `relaunch::restart` start
   `ts-launcher-fake relaunch ... --reason plugin --plugin quietoffer@0.2.0` from a
   `ts/launcher.toml` the test writes. The store then holds 0.2.0 only; 0.1.0's directory stays
   until the next start.
6. A server with `sv_client_version ">=99.0.0"` offering 0.2.0 (port + 5): at start 0.1.0 is
   removed and 0.2.0 loads; the offer answers `have`, `tspkg list` names `quietoffer@0.2.0`, and
   op 5's card, answered "Restart to update", sends `tspkg tsrs <version> restart` and starts the
   fake launcher with `--reason port --port-version >=99.0.0`.

Enforcement (issue #302, client-plugins.md §13.2, §13.3), on port + 6, with the loaded
`quietoffer` (the quiet stub prints `quiet stub suspended: <reason>` and `quiet stub resumed after
<n> callbacks while suspended`, counting its `frame` and `redraw` calls in between):

7. `[[block]] id = "quietoffer" versions = "*"` (suspend) beside the 0.2.0 offer,
   `sv_plugin_check_time 5`: the server logs `quietoffer 0.2.0: [[block]] quietoffer *: suspend`
   and sends op 4; the client suspends it with the block's reason and answers `suspended`;
   `cl_plugins_list` shows "loaded, suspended by this server (xtask block)"; the script's
   `disconnect` resumes it with 0 callbacks in between; nobody is kicked.
8. The same block with `action = "kick"`: op 4 kick, and the server kicks with "This server blocks
   the client plugin quietoffer 0.2.0: xtask kick"; the client prints that the server doesn't
   allow it.
9. A required `match = "exact"` offer of 0.3.0, `sv_plugin_required_kick 1`,
   `sv_plugin_check_time 3`, `sv_plugin_download_time 10`: the update card ("0.2.0 -> 0.3.0")
   answered "Install update", `restart`, one restart card (answered "Later"), op 3 `require
   =0.3.0` for the loaded 0.2.0, and the kick after `sv_plugin_download_time` naming the plugin.
10. 0.3.0 loads at the next start; a server offering only `quietother` with `[policy] unlisted =
    "suspend"` suspends `quietoffer` 0.3.0 as unlisted ("not allowed on this server"), and the
    other offer is answered as usual; its card is answered "Never from this server", and the
    store then holds that block.

The plugins panel (issue #311, client-plugins.md §8), on the same port with no offers: the script
runs `cl_plugins_menu`, and the hook (`crates/client/src/plugins/menu/e2e.rs`) records each row as
`kind=menu id=<id> buttons=<ops> answer=<op> files=<n> <summary>` and does the file's
`menu:<id>=<op>` as the confirmation's click does (`kind=menu-done id=<id> <result>`):

11. The panel lists the loaded `quietoffer` 0.3.0 with its 4 files and only "revoke" (no delete
    while loaded), and the blocked, not installed `quietother` with only "unblock"; after revoking
    and unblocking neither is in the store, `quietoffer` stays installed and loaded ("stays loaded
    until the game exits"), and `cl_plugins_list` shows `quietoffer 0.3.0: not approved; loaded`.
12. The test blocks `quietoffer` everywhere in the store (so the next start skips it without a
    prompt); it doesn't load, the panel offers "unblock" and "delete", and deleting removes
    `cl_plugins/quietoffer/` and keeps the block.

Run under a private X server when there's no desktop (`DISPLAY=:90 SDL_VIDEODRIVER=x11`, no
`WAYLAND_DISPLAY`). Client logs go to `target/version-client-<root>-tspkg-*.log`. Not in `all`:
run it after changes to `plugin_offers`, `shared::protocol::tspkg`, `ts_client::plugins` or the
connect path. Not covered: a real mouse click on the cards and the panel (the hook answers them
without drawing; #318), the panel's drawing and paging in game, downloads over HTTPS, and Windows
clients.

**FWGS servers drop user messages to clients that aren't spawned** (`SV_Multicast`), so the
server sends op 5 and the offers at the player's first `PlayerPostThink` after
`ClientPutInServer`, not from `ClientPutInServer` itself (#317; before that, checks 1 and 2 of
this test failed on the `fwgs` root).

**2026-10-18** (dev profile, Xvfb `:90`): on `fwgs`, `rehlds-linux` and `hlds25-linux` every check of 1, 2
and 4–6 passes, and 3 for the port client. "3: the original client joins" fails on `rehlds-linux`
and `hlds25-linux` under Xvfb: TS 3.0's client (FWGS 0.21 win32, Wine) connects but never enters
the game, with or without this change's server part (checked by hand with it reverted), so the
Wine client under Xvfb is suspect; it isn't sent anything (#320).

**2026-10-18, enforcement (#302)** (dev profile, Xvfb `:92`): on `fwgs`, `rehlds-linux` and
`hlds25-linux` every check of 1–10 passes but "3: the original client joins" on the two Steam
engines (#320, unchanged); "3: the policy doesn't kick the original client" passes there.
`cargo xtask client-plugins fwgs` passes.

## 3. Results

### 3.1 Final run (all work packages merged)

`cargo test`: all suites pass (162 tests). Builds for host, `i686-unknown-linux-gnu` and
`i686-pc-windows-gnu` have no warnings. The server matrix below passed with both debug and
**release** builds (`cargo xtask --release test --map ts_lobby`: all six roots, bots scoring).

**Dedicated servers** (`cargo xtask test --map ts_awaken`, 2 bots, 25 s; also passed earlier on
`ts_lobby`):

| Root | Result | Bot scores after 25 s (A2S_PLAYER) |
|---|---|---|
| `fwgs` | pass | 0 / 0 |
| `fwgs-win` | pass (bots counted from the log; this engine's query doesn't count them) | 0 / 0 |
| `rehlds-linux` | pass | 1 / 0 |
| `rehlds-win` | pass | 0 / 0 |
| `hlds25-linux` | pass | 0 / 0 |
| `hlds25-win` | pass | 0 / 1 |

With 4 bots for 90 s on FWGS the scores were 3 / 0 / 1 / 0 (kills vary between runs, 0–7).

**Rust client joining each server** (`cargo xtask client`: FWGS amd64 client with the Rust client
library, GoldSrc protocol for ReHLDS and HLDS; server with 2 bots, 60 s):

| Server root | Result |
|---|---|
| `fwgs` | pass |
| `rehlds-linux` | pass |
| `rehlds-win` | pass |
| `hlds25-linux` | pass |
| `hlds25-win` | pass |

The client reaches the game: the MOTD, the TS loadout menu, the weapon grid with TS sprites, free
slots, "Press Fire To Play!", and the bots ([screenshot](img/rust-client-vs-rehlds.png)).

**Alive gameplay** (FWGS listen server, scripted: spawn, `ts_give 13_8 1_0`, fire, reload, switch
to pistol): AK47 view model with scope, shells, TS crosshair with spread ring, HUD with health,
stance, free slots and weapon panel. The clip counts down from client prediction, and the client
log has no prediction errors or panics ([screenshot](img/alive-ak47-fwgs.png)).

**Wire compatibility with the original TS 3.0 client:** the unmodified TS 3.0 `client.dll`
(SHA-256 `1811bd4f…`, in FWGS 0.21 win32 under Wine) joined the Rust server on ReHLDS Linux. It
showed the VGUI MOTD, the loadout menu, the weapon grid fed by our weapon messages, "Free Slots",
and the chase camera on a bot, with no user-message errors in its log
([screenshot](img/original-client-vs-rust-server.png)).

**Windows client library:** the Rust `client.dll` and `mp.dll` load and render in FWGS 0.21
win32 under Wine (listen server, `ts_arena`).

### 3.2 Not validated

* Restart and reconnect (issue #301): `cargo xtask relaunch` passes on `fwgs` and `fwgs-win`
  (Wine) since 2026-10-19 (§2.r). Not validated: native Windows (the self-relaunch is on for
  `FwgsWindows` from the Wine run alone), Steam HL25 (#307), the real tsmod-launcher (#308) and
  the drawn card windows (#309).

* `cargo xtask plugins` (the `tsmod_*` loader and the `tsmod_test` bot backend on all seven roots,
  [implementation/bot-plugins.md](implementation/bot-plugins.md) §6) was added with issue #104 and
  not yet run in this document's runs; its checks are unit tested against a synthetic log.

* **Client plugin packages** (`crates/plugin-pkg`, issue #296): unit tests only (`cargo test -p
  plugin-pkg`): the manifest and path rules, crafted zips (traversal, symlinks, duplicate and
  case-colliding names, extra, missing and tampered files, oversized entries), every `Change`
  variant against a real unpacked install in a temporary directory (including an edit that keeps
  the size and mtime), the store round trip and `.bad` fallback, signatures,
  countersignatures, key changes and downgrades. Nothing loads plugins yet; the loader and the
  e2e runs of `implementation/client-plugins.md` §11 come with it. Not run on Windows (the
  Windows cross-builds are only compiled and linted).

* **Save games and level transitions** (#104): the save/restore code is covered by unit tests
  only (`cargo test`: the codec's byte layout, every linked class walked against TS's save tables,
  every class round-tripped with all fields filled, the global state's blocks). No xtask scenario
  saves, loads or crosses a `trigger_changelevel` yet, and no save written by TS's own server has
  been loaded into the port (or the other way round).
* A Steam GoldSrc **client** beyond the scripted HL25 Linux runs in §5 (listen server, and
  joining HL25 and ReHLDS Linux dedicated servers): the HL25 Windows client (#25), a real keyboard and mouse on either, the in-game menu opened with a real Esc
  key, and muting a second human in the GameUI player list. The pre-anniversary
  client (8684) is out of scope.
* Real keyboard and mouse (all client runs are scripted), including the Steam mouse paths in
  `input.rs`.
* Joysticks (Steam GoldSrc only: WinMM on Windows, SDL2 game controller on Linux; FWGS handles
  them in the engine).
* Voice chat with real microphones: head icons, speaker labels, the talk icon, scoreboard voice
  icons, squelch-mode muting and `voice_ban.dt`, and that Steam HLDS relays voice now that the
  server opens the streams (`Voice_SetClientListening`).
* Native Windows (only Wine).
* Long sessions, many players, and real network latency.
* The TS studio renderer and motion-blur ghosts (issues #2, #3) after they landed: unit tests
  only so far. Since the client now draws every studio model, the first `cargo xtask play` /
  `client` run should check that world models, the view model, players and their `p_` weapons
  still render, then the torso/legs split, corpses and ghosts (steps in
  `implementation/client-rendering.md`, "Test results").

* Drowning, slime and time-based damage (issue #38): unit tests only. In-engine: stay under
  water deep enough to submerge (candidates with `!` water textures: `ts_bikini`, `ts_mathar`,
  `ts_greenhouse`; depth not checked) for over 12 s and expect 3, 4, then 5 damage per second, a
  `pl_wade1` gasp on surfacing and 10 health back every 2 s. `SS_HQ_f3` has a slime
  `func_water` (skin -4, 4 × waterlevel per frame). No map in `~/dev/hl/ts/ts/maps` has lava or
  a poison `trigger_hurt` (`ts_gpc` has a nerve-gas one, which only counts down), so poison
  needs a test map.

* The x87 rounding of the damage, projectile and pellet code (issues #80, #89, #129, #138, #150,
  #161; `implementation/weapons.md` §5): unit tests pin one input per site whose bits differ
  between the builds or from the old `float` arithmetic, worked out from the disassembly. No
  in-engine comparison against the original binaries' outputs was made. Known limits: the pellet
  offsets use the platform's `sin`/`cos`, which can differ from the x87's `fsin`/`fcos` in the
  last bit, and the think timers and stock-entity damage code follow mp.dll even with
  `ts_pm_arith gcc`.

* The plugin API's setters in a running game (`implementation/plugin-api.md`): unit tests cover
  the logic, and `CreateInterface` was called outside the engine. No AMX Mod X module has driven
  them in-engine yet; that is for the `tsmod-amxx-rs` modules. The entity hooks (version 2) ran
  in-engine only with `ts_dev_hamlog` on a FWGS dedicated server with bots (§7 of that doc); the
  hamsandwich port in `tsmod-amxx-rs` is to drive them under AMX Mod X. Version 4's gun and query
  hooks and its team scores, objectives and entity creation are unit tested; in engine, `cargo
  xtask plugins`' stub checks `Ham_Item_PreFrame` on its bots' guns, `create_entity` and the
  objective list (empty on ts_dojo). Version 6's per-frame queries, `run_cmd`'s view, `msec` and
  result, `entity_key` and the touch, damage, shot and pickup callbacks are unit tested where the
  logic allows, and `cargo xtask plugins`' stub checks them in engine (bot-plugins.md §6). The private-data guard was checked
  under AMX Mod X on ReHLDS Linux and Windows (`ref/amxx-testenv`: hamsandwich fails cleanly,
  fakemeta pdata reads zeroed padding), not under xtask.

### 3.3 Skeleton milestone (before the gameplay packages)

The server library loaded on all six roots. The FWGS client joined ReHLDS (`TSHealth` shown in
the HUD), and the Windows client library loaded in FWGS win32.

### 3.4 Weapon effects against the original TS client

The original TS 3.0 client (`~/games/xash_win_orig`, FWGS 0.21 win32 under Wine, unmodified
`client.dll`) joins a port listen server on native FWGS (`connect 127.0.0.1:<port>`, Xash
protocol). The host runs an `xtask`-style script; `ts_observe 2 5 1` puts the original client in
in-eye mode on the host, who fires at the dojo's front wall from `100 -760 60` (yaw 70); the
original client's own cfg takes a screenshot every few frames. Findings:

* **Decals and impacts.** The original client shows `{shot` decals on the wall where the host's
  shots hit (events from the server carry the real origin and angles); the port's host shows the
  same decals, sparks, debris and smoke after the playback fix. Before it the port showed none at
  the wall: its predicted shots went to the world origin.
* **Flash.** In first person TS's flash comes from the per-player attachment cache, which for the
  local player holds the view model's muzzle on both engines (`client-rendering.md`, "Event
  parameters"); the port's log shows the cached point and the view model's attachment 0 are the
  same. In-eye, the original's flash glows in the middle of the view, away from the barrel (the
  target's cache is its origin); the port flashes at the view model's muzzle there (deviation).
* **Not done:** firing from the original client itself. Its anti-script gate needs a real key or
  click, and keys sent into the Wine window (`SendInput`, posted `WM_KEYDOWN`) didn't reach it.
* Seen on both clients, not investigated: the in-eye view pitches up, away from where the host
  aims, after `ts_tp` set the host's view.

### 3.5 TS's bot AI

The port of TS's bot AI (`implementation/rules.md` §10) was checked three ways:

* **Unit tests** (`cargo test -p ts-server bots`): the node graph against all 43 shipped `.ndm`
  files and `FindPath` against the decomp's replica on 30,809 node pairs over five maps (all
  pairs of ts_dojo and ts_japa), every one matching; the text files, name picker and brain
  helpers.
* **In engine** (`cargo xtask bots`, part of `play`), on both FWGS roots: `testpath` prints
  TS's paths; four `addbot` bots roam ts_dojo over the node graph (TS's `(debug)arrived` print
  for every path they plan: 15–20 in 40 s), move 450–1350 units within a life and kill each
  other (2–4 kills); with `resetnodes` and `botlearn 2` their walk learns a new graph. The play
  scene's four quota bots (now TS bots too) scored 9 bot-on-bot kills in 70 s with TS's random
  loadouts (Benelli, Glock-20C, knives, ...), and bot chat from `bot_say.txt` appears. The
  dedicated-server matrix passes with TS bots thinking on all six roots (port addition), and
  each root logs `[ts] bots: 64 names, chat lines [25, 29, 42], <n> nodes` at map start.
* **Against the original** TS 3.0 (FWGS win32 under Wine with TS's own `mp.dll` and client,
  `~/games/xash_win_orig`, a temporary cfg only): `addbot` bots fight there too, and with
  `botlearn 2` its `testpath 89 25`, `testpath 0 50` and `testpath 10 100` print
  `path:89,88,25`, `path:0,47,50` and `path:10,15,63,84,100` (read from screenshots: the
  original client shows those lines on the HUD only). The port prints the same three.
  `scripts/ndm.py` gives `89,87,25` for the first: it rounds the walk heuristic to a float,
  which the x87 code (`fsqrt` straight into `__ftol`, 0x100719eb) doesn't.

### 3.6 The original TS client in 2.1 mode

Against the Steam engines' dedicated servers `cargo xtask ts21` checks it (below, §2.y). Against
FWGS it was checked by hand (it needs the Wine root `~/games/xash_win_orig` and a window). A native
FWGS dedicated server with the port (`cargo xtask install fwgs`, then in
`~/games/xash`: `./xash -dev 1 -game ts -port 27142 +maxplayers 8 +sv_lan 1 +servercfgfile
<cfg> +map ts_dojo`, the cfg holding `ts_gameplay 21`), and the original TS 3.0 client
(`wine xash3d.exe -game ts -dev 2 -log -windowed +exec <cfg>` in `~/games/xash_win_orig`, the cfg
`connect 127.0.0.1:27142` without `gs`, then waits, `screenshot`, `quit`):

* **2.1:** the client's `engine.log` shows `Server was reject the connection: Invalid Version:
  Running 2.1 mode. Upgrade to the rust port at tsmod.org`, and the server logs `[ts] gameplay:
  refused a client in ruleset 21 (Other): ...`. FWGS dedicated runs the server config once before
  the first map as well, so `ts_gameplay 21` from it already applied to that map (`ruleset 21 for
  this map`, no reload).
* **3.0** (the cfg holding `ts_gameplay 30`): the same client joins, loads ts_dojo and shows the
  buy menu; the `SrvSett` keys 4..7 it ignores change nothing.

The port's own client joining a 2.1 map is covered by `cargo xtask play` (the host reconnects at
the reload under 2.1 and plays on).

**The Steam engines** (`cargo xtask ts21`, §2.y; the same client with `connect ... gs`): ReHLDS
(`rehlds-linux`) prints `Invalid Version: Running 2.1 mode. Upgrade to the rust port at tsmod.org`
to the client, HL25 (`hlds25-linux`) `Server issued disconnect. Reason: Server refused connection
because:  Invalid Version: Running 2.1 mode. Upgrade to the rust port at tsmod.org`; both servers
log the refusal. The Windows Steam roots weren't tried (the same engine code, under Wine). The
port's FWGS client joins both and logs `[ts] gameplay: the server runs ruleset 21`.

### 3.7 TS 2.1 mode on every dedicated server (issue #103)

`cargo xtask ts21` (§2.y) passed on all seven roots, every scene, and both client checks on
`rehlds-linux` and `hlds25-linux`; the full run took 5 min 46 s (the servers 233 s). Findings:

* The server config's `ts_gameplay 21` reloads the first map once on the four Steam roots, and
  FWGS (all three builds) starts it under 2.1. rcon `ts_gameplay 30` / `21` reload once each; no
  root reloaded again.
* Rounds, team cash and side swaps, LMS, The One, model teamplay and the objective manager behave
  as `implementation/ts21.md` describes. Two things that look wrong and match 2.1:
  `respawnnumber` and `playerrespawn` count lives (the default `respawnnumber 1` is no respawn in a
  round TDM round), and in The One mode the One's and the others' teams must be in `mp_teamlist`
  (the content's `server.cfg` lists `team1;team2;team3;team4`, which puts everyone on those).
* `MecklenburgV_Rain_Final` doesn't load on ReHLDS at `maxplayers 8` under either ruleset (§4).

## 4. Pitfalls found during setup

| Symptom | Cause | Fix |
|---|---|---|
| Stock HLDS: `Couldn't get DLL API from <garbage>` | Stock HLDS (8684 and HL25) strips `_i386` from `gamedll_linux` and loads `dlls/ts.so`. ReHLDS loads the name as written. | xtask installs both `ts_i386.so` and `ts.so`. |
| "HL25" install reported build 8684 | steamcmd remembered `-beta steam_legacy` for app 90 from an earlier run | Use a fresh steamcmd copy and `-beta public` explicitly. Check `BetaKey` in `steamapps/appmanifest_90.acf`. |
| Windows HLDS under Wine: no output, no answer | First-run Wine prefix setup (Mono/Gecko dialogs) blocked startup; a half-created prefix then failed to load `kernel32.dll` | Create the prefix headlessly (§1.1). `hlds.exe -console` writes to its own console window: use `-condebug` (`qconsole.log`). |
| Windows HLDS answers only after 20–30 s on the first start | Wine plus `server.cfg` plus VAC initialisation | xtask waits up to 120 s for Windows roots. |
| FWGS 0.21 win32 doesn't answer A2S_INFO | Old FWGS answers only the Xash `info` query | xtask falls back to `info 49`. |
| Release `mp.dll` never loads on HL25 Windows (crash in `swds.dll` right after `LoadLibrary`); the debug build works | The Steam engine reads the DLL's export names from the file: it finds the section *named* `.rdata` and translates the export RVAs with it (MSVC puts exports there). MinGW emits `.edata`, so the engine walks garbage. The debug build only survived because the garbage happened to be readable memory. | xtask renames `.rdata` to `.rodata` and `.edata` to `.rdata` in every Windows DLL it installs or packages (`msvc_style_export_section`). |
| Bots never join on GoldSrc; fine on FWGS | Command-line `+cvar` runs before GameInit registers the game's cvars | xtask writes the quota into `xtask_server.cfg` and passes `+servercfgfile`. |
| HL25 crashes after the last human leaves | HL25 drops every bot then; driving a dropped bot with `pfnRunPlayerMove` crashes `SV_RunCmd` | Bots are driven only while they have a live user id. |
| Crash in `pfnSetClientMaxspeed` (FWGS) after a bot is kicked; the Windows dialog shows `Host_Shutdown (xash.dll)` under `mp.dll` frames | A dropped client keeps `FL_CLIENT`/`FL_FAKECLIENT`, and FWGS still maps its edict to the freed slot: `pfnGetPlayerUserId` returns the old id, and `pfnSetClientMaxspeed` dereferences the slot's NULL edict when the bot respawns | `ClientDisconnect` clears both flags, so dropped players drop out of `players()`, `is_player()` and the bot list, and `rules::set_client_maxspeed` skips the engine call for them (TS's own player tests still see them: `has_player`, `ts_flags`, `slot_players`) |
| `addbot` bot kicked half a second later | `ts_bot_quota` (default 0) kicked bots above the quota | The quota counts and kicks only its own bots; `addbot` bots (listen servers, as TS) are left alone |
| `addbot` did nothing on a listen server ("Create Server") | The port followed the Linux `ts_i386.so`, a dedicated-only build whose `addbot` only prints "Bots only in listen servers." | `addbot`/`addcustombot` add a bot as TS's Windows `mp.dll` does on a listen server, and (a chosen deviation) on a dedicated server too. `xtask test` runs `addbot` from every dedicated server's config and fails unless a bot connects |
| Escape did nothing in game (FWGS 0.21 win32, Steam GoldSrc) | Those engines run Escape's hard binding, `cancelselect`, instead of opening the menu; TS's `cancelselect` sends `escape` when no weapon menu is open, the port's only closed the weapon menu (newer FWGS opens the menu itself) | `cancelselect` sends `escape` as TS (`hud::cancel_select`); `xtask play` checks it |
| Quota kicks never happen in a scripted test | `pfnServerCommand("kick ...")` is appended to the console buffer behind the rest of the running `wait` script | The listen script ends right after `ts_bot_quota 0`; xtask stops the game 10 s later |
| `SteamAPI_Init() failed` in HLDS logs | Normal for dedicated servers (they use the game-server API) | none |
| Steam HL25 client: the game runs, but the world is black and the "Loading… Parsing game info" dialog never goes away (issue #62) | A client without `IClientVGUI` is treated as VGUI1, and the engine then only draws the world when the client's VGUI1 viewport calls `VGui_ViewportPaintBackground` (TS's does). The port has no VGUI1 viewport, so `V_CalcRefdef` and `HUD_Redraw` were never called and the screen kept the dialog's last frame | `client-dll` exports `ClientFactory`/`CreateInterface` and an `IClientVGUI` with `UseVGUI1() == false` (`implementation/client-hud.md` §7) |
| Steam HL25 client: `ts_tp`/`ts_face` (and spawn angles) ignored, so scripted shots miss; `ts_test_view` shows the yaw stuck | While a menu owns the cursor (the MOTD stays open in scripts) `CL_CreateMove` held the view angles, overwriting the server's `svc_setangle`. Not seen on FWGS | Only the player's own turning is held; angles the engine changed since the last `CL_CreateMove` replace the held ones (`input::menu_hold`) |
| Steam client on a dedicated server: after `changelevel` the player stays an observer, clicks do nothing | The 0.2 s click timer (`respawn` on `+attack`) is in client time, which restarts with the map, so the old map's timer blocked every click | `spectator::vid_init` zeroes it, as TS's `CHudSpectator::VidInit` does |
| Third person after every death and respawn | Entering observer mode saved the death cam's third person and leaving restored it | Leaving re-applies `cl_thirdperson` (TS `Spectator` handler `10065250`) |
| `rcon changelevel` from the Steam client changes the map twice; the client then stalls on "Retrying" (`Skipping reconnect ..., no pvPrivateData` in the server log) | The HL25 client sometimes sends an `rcon` twice (seen for `ts_bot_quota` and `changelevel`) | Drive the dedicated server's console through its stdin (§5), not `rcon` |
| `ts_bot_quota` on `maxplayers 1` prints `[ts] bots: no free client slot` every 0.5 s | The quota retried with a message each time | A failed add is reported once per streak |
| Steam client can't join an FWGS dedicated server (`Retrying 127.0.0.1:...`, nothing in the server log) | FWGS servers speak protocol 49 only; the Steam client is protocol 48 | none (engine limitation) |
| `PF_MessageEnd_I: Unknown User Msg 114` ×3 on a Steam listen server (`developer 1`) | Message 114 is `SrvSett`. The first `CTSGameRules::Think` broadcasts keys 0–2 (`realbullet`, `usecash`, `ammocount`) before any client has received the user-message table, and the engine drops a message it hasn't registered with a client yet | Harmless: each client gets all four `SrvSett` values in `InitHUD` |
| Four more `addbot`s in the play scene: two fail with "Could not find any free client slots" | The quota's kicks of its four bots wait behind the script (above), and `maxplayers 8` | The TS bot check runs as its own listen server (`cargo xtask bots`) |
| `testpath`'s `path:` lines show up after later lines (the bots' joins) in the client's console | The client's say-text display, not the server (it prints at once) | The check searches the whole scene |
| `testpath 89 25` on ts_dojo: the port prints `path:89,88,25`, the decomp's `scripts/ndm.py` replica `89,87,25` | The replica rounds `FindPath`'s walk heuristic to a float before truncating it; `mp.dll` keeps it on the x87 stack (and the original TS 3.0 prints `89,88,25`) | The unit tests' reference data (`bots/testdata/gen.py`) is generated with that rounding removed |
| The original TS client shows `testpath`'s and `botlearn 2`'s replies on the HUD only, not in the console log | Its say-text print doesn't echo `TextMsg` talk lines to the console | Read them from a `screenshot` (the original's MOTD and buy menu cover the chat: `motdfile` to a missing file, `slot10`) |
| FWGS dedicated server: the first command after `exec server.cfg` in xtask's configs does nothing (`"sv_maxspeed" changed to "330ts_gameplay"`) | The content's `server.cfg` has no final newline, and FWGS's `exec` inserts the text without ending it, so its last line runs into ours (issue #187) | xtask's configs follow `exec server.cfg` with an empty line (`roots::EXEC_SERVER_CFG`) |
| `rcon changelevel` on ReHLDS's Windows release: the new map's `[ts]` lines are missing from `qconsole.log` | A command run from rcon prints into the rcon reply (`SV_BeginRedirect`); a map load it starts prints there too | `cargo xtask ts21` writes `changelevel <map>` into a config and sends `rcon exec <config>`, which queues it for the next frame |
| `MecklenburgV_Rain_Final` on ReHLDS: `ED_Alloc_internal: no free edicts`, under 3.0 too | The map has 1046 entities; ReHLDS allows `edicts + 15 × (maxplayers − 1)` edicts, where `edicts` comes from `liblist.gam` (900 when absent: 1005 at 8 players), and TS 3.0's `liblist.gam` doesn't set it | Deviation: the port's `liblist.gam` sets `edicts "1800"`, the same as `gameinfo.txt`'s `max_edicts` for FWGS, so the map loads without `-num_edicts` |
| `xtask play`: the host sometimes spawns with nothing after the scripted buy (`spawned (#1) with []`, issue #102), mostly on fwgs-win | The compositor resizes the Wine window a second or two after the join (the engine re-renders its fonts at a new size); FWGS then calls `HUD_VidInit`, which closes the buy menu as TS's does (`CHudTSWeapons::VidInit` `0x1004cb50` re-runs the menu's `Init` `0x10036200`), and the `slotN` keys went nowhere | The join sends `buy` (reopens the menu while observing) and the keys twice; the second pass submits the same loadout |
| `xtask play`: no `[ts fx] laser other` line for the bot (issue #179) | The bot's random loadout (seen: Golden Colts, a knife and an M16, with their ammo) left fewer free slots than a Beretta's 10, so `ts_as 2 ts_give 2_2` was refused (`PickUpWeapon`'s slot check) and the bot kept a gun without a laser | The bot gets the Berettas as its loadout (`ts_as 2 rst_cfg`, `ts_as 2 tkw 2_2 2_0`) before its first spawn |
| `ts_hurt` on a player who has just respawned does nothing (`applied=false health=100`) | Spawn protection (`protect_until`) | Scripts hurt again a second later (`ts21::Server::hurt`) |
| A scripted client run does nothing: no `[ts] client` lines, only the server's, in `engine.log`, which starts with `SDL_Init failed: No available video device` | The session has no display (an agent's shell, ssh): FWGS then runs as a dedicated server (and Wine's `hlds.exe` dies with `CTextConsoleWin32::GetLine` at shutdown). The game-running commands now stop at once with exit 2 when neither `DISPLAY` nor `WAYLAND_DISPLAY` is set (#329) | Run an Xvfb (`Xvfb :87 -screen 0 1280x800x24`) and start xtask with `DISPLAY=:87 SDL_VIDEODRIVER=x11` and `WAYLAND_DISPLAY` unset |
| `savenodes` in a scripted test would overwrite the content's `.ndm` | The roots' `ts/maps` is a symlink to the content directory | The scripts never run `savenodes`; `resetnodes` is followed by `loadnodes` |

## 5. Steam Half-Life client (HL25, issues #23 and #5)

`cargo xtask steam` (issue #5, old #67; `xtask/src/steam.rs`) runs the Linux 25th-anniversary
Steam client (32-bit `hl_linux`, engine build 10211) with a script, as `xtask play` does with
FWGS. The Steam client isn't one of xtask's roots: it needs Steam running and logged in, and only
one copy of the game can run at a time.

```sh
cargo xtask steam [listen]        # listen server: the play scenes, changelevel, menu angles (~7 min)
cargo xtask steam hlds25-linux    # the Steam client joins a dedicated server (~4 min each), then a 2.1 one
cargo xtask steam rehlds-linux
cargo xtask steam listen hlds25-linux   # several, one after the other
```

What it does:

1. Finds Steam in `~/.local/share/Steam` or `~/.steam/steam`, and the library folder with
   Half-Life (app 70) from `steamapps/libraryfolders.vdf`. `TS_STEAM_HL=<dir>` points it at
   another Half-Life folder. It takes `Half-Life/ts/xtask_steam.lock` (a second `xtask steam`
   stops with a message; a lock whose process is gone is taken over), and stops if `hl_linux`
   is already running.
2. **Steam login.** The logged-in account comes from `ActiveProcess/ActiveUser` in
   `~/.steam/registry.vdf` where Steam writes it; the current Linux client doesn't, so the
   fallback is the last state line of `Steam/logs/connection_log.txt` (`[Logged On, ...]
   [U:1:<account id>]`). If Steam isn't running, xtask starts it with `steam -silent` (in its own
   process group; it keeps running afterwards) when it can log in by itself: `AutoLoginUser` in the
   registry with `RememberPassword 1` in `config/loginusers.vdf`. It waits up to 3 min for the
   login. Without a saved login, or when the login doesn't happen in time (password, Steam Guard,
   an update), it stops with a message saying to log in to Steam and run it again.
3. Assembles `Half-Life/ts` the way `cargo xtask install` assembles a root: it links the TS
   content (`TS_DIR`), copies `game/ts`, and installs the i686 Linux build as
   `cl_dlls/client.so`, `dlls/ts_i386.so` and `dlls/ts.so`.
4. **Friends status: invisible while testing** (user requirement), so friends don't see the
   account playing. See §5.2.
5. Writes `ts/xtask_steam.cfg` and starts the game through the Steam runtime from the Half-Life
   folder, never with `steam -applaunch` (it hangs while the desktop is locked):

   ```sh
   SteamAppId=70 SteamGameId=70 ~/.local/share/Steam/ubuntu12_32/steam-runtime/run.sh \
     ./hl.sh -game ts -windowed -width 1280 -height 800 -condebug [-port 27170] +exec xtask_steam.cfg
   ```

   xtask follows `Half-Life/qconsole.log`. The script ends with `ts_dev_touch xtask_steam_done`.
   A few seconds after that marker appears, or at the time limit (20 min listen, 10 min
   dedicated), xtask stops the game (`pkill -x hl_linux`) and removes the cfg and the marker. The
   game renders and takes screenshots while the desktop is locked.
6. Moves the screenshots (`ts/HalfLifeNN.tga`; older ones are deleted before the run) to
   `target/steam-<listen|root>-NN-<scene>.png` (ImageMagick's `magick`; the `.tga` is kept
   without it). Each is matched to its scene by the `echo XTASK: <scene>` before its
   `screenshot`. Logs: `target/steam-<listen|root>.log` (client console),
   `target/steam-<root>-server.log`, the client's stdout in `target/steam-<...>-stdout.log`, and
   `target/steam-launch.log` when xtask started Steam.

The scripts reuse `play`'s scene helpers (`xtask/src/main.rs`). HL25 runs in real time (xtask
doesn't use its `sys_timescale`), and `play`'s `secs()` counts frames for FWGS at 2x, so the Steam
prologue makes the `w10`/`w100` aliases twice as long: at `fps_max 100` a `secs(n)` is then `n`
seconds here too.

**Listen server** (`maxplayers 8`, `developer 1`, `sv_lan 1`, port 27170): `play`'s scenes and
checks (§2) on `ts_dojo` in `play`'s order: join and buy, wall shots, a stopped bot, lasers and
akimbo, power-up, grenade, kill, kick, death and respawn, four bots. Then, with the bots stopped,
the Steam scenes below, then `play`'s last ones: `ts_bot_quota 0`, `addbot`, Escape. Not run on
Steam: `play`'s gameplay-ruleset scene (`ts_gameplay 21`/`30` reload the map with a server
command, which GoldSrc appends to the console buffer behind the running script, so the reload
only happens after the script: in a first run it then reloaded `ts_dojo` twice and the quota's
kicks never ran; `xtask ts21` covers the reload on the Steam engines), and `meck` and `bots`
(`play`'s other two runs).

**Dedicated server** (`hlds25-linux`, `rehlds-linux`; port 27160 and up): xtask starts the root's
server on `ts_dojo` with `+servercfgfile xtask_steam_server.cfg`, which runs `server.cfg`, then
`sv_cheats 1`, `ts_bot_quota 0`, `ts_bot_stop 1`, `developer 1` and console-only logging
(`mp_logfile 0`, `log on`), on every map. The client script `connect`s and marks server commands
with `echo SRVCMD <command>`; xtask writes each one to the server's stdin as soon as it appears in
the client log. It doesn't use `rcon`: the HL25 client sometimes sends an `rcon` twice, which turns
one `changelevel` into two and strands the client. The `ts_status` dumps and the end marker are
server commands too. xtask rejects `fwgs` (FWGS servers speak only protocol 49, the Steam client
48: §4) and the Windows roots (it drives the server console through stdin).

| Scene | How | Passes when |
|---|---|---|
| Join | `connect`, wait for the buy menu, buy the AK47 with the `xtask play` keys, click after the respawn wait | Server: `[ts] <host> spawned (#1) with [13]` |
| Bot | `ts_bot_quota 1` (the bot stands still) | `[ts] bots: added` |
| Chat | `say`, `say_team` | Both in the server log (`say "..."`, `say_team "..."`) and in the client console |
| Scoreboard | `+showscores` | (screenshot) |
| Kill | `ts_tp` both players, `+attack` with `ts_face 1 2` every 0.2 s for 5 s | The client's kill notice `<host> killed Bot ...`, and a frag in `ts_status` |
| Death and respawn | `ts_hurt 1 500`, click through the death cam, click after the wait | Dead with a death, then a second spawn line and alive |
| Menu angles, `changelevel` | the Steam scenes below | |
| End | | The marker was reached, client and server still run, no panic or crash in either log |

**The Steam scenes** (both modes) check the regressions that only showed on the Steam client:

| Regression | Scene | Passes when |
|---|---|---|
| Black world, loading dialog stuck (old #62: the client must export `IClientVGUI`) | screenshots after the respawn and on the new map | at least half of each screenshot's pixels have a channel above 4 of 255 (`lit_share`; the dim `ts_awaken` spawn scores 96%, 78% under the MOTD's dimming; a black world leaves only the HUD) |
| Third person after a respawn (old #23: leaving observer mode restored the death cam's) | `ts_test_view` after the first spawn, the respawn and the spawn on the new map | `third false` each time |
| Server view angles held while a menu owns the cursor (old #23: `CL_CreateMove` overwrote `svc_setangle`) | `ts_face 1 100000 0 0`, `ts_test_view`; `motdfile motd.txt` and `cmd motd` (HL25's own `motd` command only prints the client's copy; the MOTD opens and stays, a script can't close it); `ts_face 1 0 100000 0`, `ts_test_view` | yaw 0, then yaw 90 with `menu true` (the points are far outside any map, so the yaw doesn't depend on where the host stands) |
| Click-to-spawn after `changelevel` (old #23: the click timer wasn't reset on map load) | `changelevel ts_awaken`, wait for the buy menu, buy, click | a `spawned (#1) with [13]` line after the change, and the host alive in `ts_status` |

**2.1 mode** (each dedicated mode, after its run): the root's server restarts with `xtask ts21`'s
config (`ts_gameplay 21`) on port 27180.., and the Steam client connects. Passes when the server
doesn't log `[ts] gameplay: refused`, the client logs `[ts] gameplay: the server runs ruleset 21`
(`SrvSett` 4), and the script finishes. `xtask ts21` can't catch a missing `_tsrs` key here: its
port client is FWGS, which puts `FCVAR_USERINFO` cvars in the userinfo when they are registered,
and GoldSrc doesn't (`implementation/gameplay-plugins.md` §4.2).

`ts_test_view` (a client dev aid) prints `third <bool> menu <bool> ... angles [pitch, yaw, roll]`;
`menu` is whether a menu owns the cursor.

**In `cargo xtask all`.** `all` runs `xtask steam listen hlds25-linux` (the dedicated run only
when that root exists) after `play`, **only if Steam is already running and logged in** and no
`hl_linux` is running. Otherwise it prints `== steam: skipped (<why>)` and the summary line says
`Steam client skipped`; a skip doesn't fail `all`. `all` never starts Steam itself. This adds
about 11 min to `all` on a machine with Steam logged in.

Notes:
* GoldSrc's tokenizer splits `XTASK:` into `XTASK` and `:`, so `echo` prints `XTASK : ...`. xtask
  restores the markers before the checks.
* The engine splits a `+exec` file name at a `-` on the command line
  (`exec xtask_steam: not a .cfg or .rc file`), so the cfg is always `xtask_steam.cfg`.
* HL25 prints `- <name> has left the game` for a kicked bot instead of `<name> was kicked`. The
  checks accept both.
* The engine's `exec config.cfg` and the game's `exec skill.cfg` (TS ships no `skill.cfg`, so
  Half-Life's `valve/skill.cfg` with 393 `sk_*` lines) wait in the console buffer behind the
  script. Run after it with the host in the game, each `sk_*` line goes to the server as a client
  command (TS registers no `sk_*` cvars, decomp `docs/server/architecture.md`, and neither does the
  port) and comes back as an `Unknown command` `TextMsg`; the host's reliable channel overflows
  (`Reliable channel overflowed`, `Host_EndGame`) before the quota's kicks run. A minimal script
  (`map ts_dojo`, waits, end; no bots) reproduces it, while the same burst of `config.cfg`'s
  userinfo keys alone doesn't, on `-game ts` or `-game valve`. So the listen run puts an empty
  `ts/skill.cfg` in the Half-Life folder and removes it afterwards. (On a real listen server the
  `exec skill.cfg` runs while the map loads, before the host's client is in the game.)
* `Sentence or Pitch shift ignored. > 16 playing!` (developer-only) appears while the AK47 fires.
* Compare with stock Half-Life launched the same way (`-game valve`, `map crossfire`) to tell the
  port's problems from the setup's.

**Result (2026-10-15, issue #5; Steam client build 10211, `hlds25_linux` 10211):**
`cargo xtask steam listen` passed all 23 checks (script 344 s), and `cargo xtask steam
hlds25-linux` all 10 (script 182 s; 45 server commands forwarded). Evidence from the logs:
`[ts] [MOS] Azerothian spawned (#1) with [13]` before and after `changelevel ts_awaken`;
`ts_test_view` printed `third false` after the first spawn, the respawn and the spawn on the new
map, and with the MOTD up `menu true ... angles [-0.15, 89.85, 0]` (listen) and
`[0.04, 89.92, 0]` (dedicated) after `ts_face` along +y; screenshots 94–100% lit on the
listen server, 81–100% on the dedicated one; 8 bot-on-bot kills; the quota's kicks after the
script. The friends status went online → invisible → online on every run. With the three
old #23 client fixes reverted (the menu hold, the click-timer reset, the observer-exit camera),
the dedicated run fails exactly the menu-angles, third-person and changelevel checks (the view
stays at yaw 0 with the menu up, `third true` after the respawn, no spawn on the new map), so the
scenes catch those regressions. A Ctrl-C (SIGINT to xtask's process group) while the game ran
left no `hl_linux` or `hlds_linux`, removed the lock and put the status back to online.
`rehlds-linux` and `cargo xtask all`'s Steam step weren't run this round.

### 5.2 The friends status while testing

`xtask steam` sets the Steam friends status to **invisible** before it starts the game and puts
the previous status back afterwards:

* **Reading it.** Steam keeps the status the user picked as `ePersonaState` in the
  `FriendStoreLocalPrefs_<account id>` value (JSON) of
  `Steam/userdata/<account id>/config/localconfig.vdf`, and rewrites the file within a second of
  every change (checked: `steam://friends/status/invisible` turned `"ePersonaState":1` into `7`
  one second later). The values are Steam's `EPersonaState`: 0 offline, 1 online, 2 busy, 3 away,
  4 snooze, 5 looking to trade, 6 looking to play, 7 invisible.
* **Switching.** `steam steam://friends/status/invisible`, which hands the URL to the running
  Steam and exits; afterwards `steam steam://friends/status/<offline|online|busy|away|snooze|trade|play>`.
  If the status was invisible already, nothing is changed. **If it can't be read** (no
  `localconfig.vdf`, no `ePersonaState`), it is put back to **online**, and xtask says so.
* **On failure.** The restore runs when the run ends, whatever the checks say, and when xtask
  panics (a drop guard). For Ctrl-C or a kill, xtask starts a watchdog (`sh`, in its own process
  group so Ctrl-C doesn't reach it) before switching: when the xtask process is gone, it stops
  `hl_linux` (the game runs in its own process group too), removes the lock and puts the status
  back. The normal path stops the watchdog first. Only `kill -9` of both, or Steam itself exiting
  first, can leave the status invisible.
* If xtask started Steam, Steam comes up with the stored status (usually online) and shows it for
  the few seconds between the login and the switch.

For debugging, `hl.sh` runs the game under gdb when `DEBUGGER=gdb` is set, and passes
`DEBUGGER_ARGS` through (`-batch -x <script>` with breakpoints and `run`). `hw.so` ships with its
symbols and DWARF, so breakpoints on engine functions (`VGui_ViewportPaintBackground`,
`CBaseUI::ActivateGameUI`) and `print cls.state` work.

**Earlier manual result (issue #62 fixed):** the scripted `xtask play` scenes all pass on the Steam client
(listen server, `maxplayers 8`): join and buy, spawn, slow-motion power-up, grenade, kill, kick,
death and respawn, and four bots fighting. The world, HUD, buy menu, MOTD, view model and the
slow-motion indicator draw; the console and the game menu open over the game and give the mouse back.
The listen server did not start paused. Expected log noise: `DemoPlayer::Init: couldn't get
engine interface`, `System::AddModule: couldn't initialize module (null)`, `failed to locate
sequence file ts_dojo`, `runtime sample conversion` for three TS player sounds, and
`Unknown User Msg 114` (§4). The `ts_dojo` sky is white because the map's `construct_*` sky
images are white.

### 5.1 Steam client on dedicated servers, `maxplayers 1`, GameUI

**Dedicated server.** `cargo xtask steam hlds25-linux` and `cargo xtask steam rehlds-linux` now
run these checks (§5). The table below is from the earlier manual runs (the same scenes, driven
through a fifo on the server's stdin, plus `ts_test_view` after aiming and after each spawn). The
team-menu row is still manual: set `mp_teamplay 1` on the server, `changelevel` so the rules
reinstall, then check the auto-assigned team, `changeteam` and `slot2`.

| Check | `hlds25-linux` (10211) | `rehlds-linux` | `fwgs` |
|---|---|---|---|
| Join, MOTD, buy menu, click to spawn | pass | pass | can't join: FWGS servers only speak protocol 49 (§4) |
| Chat (`say`, `say_team`) in the client and the server log | pass | pass | – |
| Scoreboard (names, scores, ping, voice column) | pass | pass | – |
| Shoot a bot dead (kill notice, frag, cash) | pass, after the menu-angles fix | pass | – |
| Death cam, respawn, first person again | pass, after the third-person fix | pass | – |
| `changelevel` to `ts_awaken`: reconnect, buy menu ("Use Last Config"), spawn | pass, after the click-timer fix | pass | – |
| Team menu, `changeteam` (`mp_teamplay 1`) | pass | not run | – |
| Logs | expected noise only (below) | expected noise only | – |

Client log noise, all also present with stock Half-Life or explained in §4: `DemoPlayer::Init`,
`System::AddModule`, three `runtime sample conversion` warnings, and `.so: cannot open shared
object file` at exit. Server log: `couldn't exec maps/<map>_load.cfg`, `Cvar_Set: variable
room_type not found` and the Steam API lines. No voice errors (voice itself was not exercised).
The HUD's additive sprites vanish over the white `ts_dojo` sky (expected).

**`maxplayers 1` listen server** (`maxplayers 1`, `map ts_dojo`): join, buy, spawn, walk, fire,
`ts_hurt` death and respawn all work; the rules are deathmatch, as in TS. (Then, `addbot` reported
`no free client slot` once; on a listen server it now adds a bot, as TS's Windows server does.) The game does not pause while the game menu or the console is open:
server time keeps running. Stock Half-Life (`-game valve`, `map crossfire`, `maxplayers 1`) with
the menu opened the same way doesn't pause either, so this is the engine's behaviour.

**GameUI player list.** Nothing on the console opens it, so the run used gdb (`DEBUGGER=gdb
DEBUGGER_ARGS="-batch -x <script>" ./hl.sh ...`; `gameui.so` has symbols): a breakpoint on the
client's `ts_test_view` command runs

```
set language c++
call ((CBaseUI*)&g_BaseUI)->ActivateGameUI()
call ((CTaskbar*)g_pTaskbar)->OnOpenPlayerListDialog()
```

The dialog lists the players, with an empty Properties column and **Mute in-game voice**
disabled. `g_pGameClientExports` is NULL: `CGameUI::Initialize` asks the client factory for
`GameClientExports001`, the port returns NULL (as TS 3.0 does, it has no such interface), and
every use in `CPlayerListDialog` checks for NULL. So the GameUI list can't mute anyone; muting
works through the scoreboard's squelch mode, as in TS. The TS menu background (`resource/
BackgroundLayout.txt`) draws over the world while the menu is up.

**Listen server again:** the `xtask play` script (`~/games/xash/ts/xtask_play.cfg`, `quit`
appended) on the final build: kill, power-up (host 0.6, bot 0.35), death and respawn, and twelve
bot-on-bot kills.
