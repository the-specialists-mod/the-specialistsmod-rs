# The Specialists 3.0, in Rust

A Rust port of **The Specialists 3.0** (TS), the GoldSrc/Half-Life mod: both game libraries,
the server (`mp.dll` / `ts_i386.so`) and the client (`client.dll`). It's ported from the
decompilation in [the-specialists-decomp](https://github.com/the-specialists-mod/the-specialists-decomp)
and is **wire-compatible with TS 3.0**: the original TS client can join this server, and this
client speaks the same protocol.

It runs on Xash3D FWGS, ReHLDS (pre-anniversary HLDS) and post-anniversary (HL25) Valve HLDS,
on Linux and Windows. The latest release is
[v3.0.8](https://github.com/the-specialists-mod/the-specialistsmod-rs/releases/tag/v3.0.8).

![The Rust client and server on Xash3D FWGS: ts_dojo, AK47 against a bot](docs/img/ts-dojo-fwgs.png)

## Status

| Area | State |
|---|---|
| Player movement (shared, predicted) | The TS `pm_shared` port: ladders, water, duck, step-up, the TS jump, friction and acceleration, the `fuser1` time scale, bit for bit in the arithmetic of each TS build. The server computes it as TS's Windows server on every platform, so every client predicts it exactly (`ts_pm_arith gcc` selects the Linux `ts_i386.so`'s instead). |
| Stunts and animation | Dives, rolls, flips, cartwheels, wall jump and climb, slide and prone, super jump, bullet-time dodge. TS animation selection. |
| Weapons | All 37 TS weapons from the decoded tables. One shared `Gun` state machine for server and client prediction: fire modes, akimbo, burst, pump, zoom, attachments. Penetration, simulated bullets, grenades, thrown blades, kung fu with blocks, dropped guns, loadouts. |
| Game rules | DM, team DM, LMS, The One. Spawn selection, scoring and awards, power-ups (slow motion, slow pause, double fire rate, infinite clip, kung fu, health, armour, super jump), slow-motion propagation, chat and chat votes (`vote map`/`stats`/`reset`), MOTD, the TS cvars, TS's bots (the Windows server's AI: `addbot`, the `.ndm` node graph, `botlearn`), HLTV director messages. |
| Map entities | 72 stock classnames with TS's changes (respawning breakables and doors, …) plus the TS entities. All 97 shipped maps load, and 88.8 % of map entities have a class. |
| Client HUD and UI | Every server message hooked. TS HUD in draw order, the loadout/buy menu, scoreboard, team select, MOTD, `ShowMenu`. TS input with `+alt1`/`+alt2`, the anti-script gate, and mouse look for FWGS and Steam GoldSrc. |
| Client view and effects | TS view (bob, client-side recoil, third person, slow-mo tint and fades), all 11 weapon events, the TS particle system and map emitters, lasers, flashlights, shadows, the TS studio renderer (split torso/legs bones) and motion-blur ghosts. |
| Save games | Save/restore of every linked class against TS's save tables. Unit tests only: no in-engine save, load or level transition yet. |
| Open work | Validation on setups not covered yet ([issues](https://github.com/the-specialists-mod/the-specialists-rs/issues)), scripted Steam client checks in xtask ([#5](https://github.com/the-specialists-mod/the-specialists-rs/issues/5)). |

Per-subsystem details, decomp mappings and gaps: [docs/implementation/](docs/implementation/).

### Where it has been tested

Every change is checked with `cargo xtask all`: a dedicated server with bots on all six engine
roots, then scripted listen-server gameplay on both FWGS roots (join, buy, spawn, wall shots,
lasers, akimbo, power-ups, grenades, kills, kick, death and respawn, bot fights, the Escape menu,
grenade kills and breakables on `MecklenburgV_Rain_Final`, TS's bot AI and node graph on `ts_dojo`). v3.0.8 passed it with the stripped
release libraries, and the 1403 unit tests pass.

| Engine | Server | Client |
|---|---|---|
| Xash3D FWGS, Linux amd64 | dedicated and listen, scripted gameplay | the Rust client: scripted gameplay; joins the FWGS, ReHLDS and HL25 dedicated servers |
| Xash3D FWGS 0.21 win32 (under Wine) | dedicated and listen, scripted gameplay | the Rust client; the **original TS 3.0 client** joins port servers (wire compatibility) |
| ReHLDS, Linux and Windows (Wine) | dedicated, bots | — |
| Valve HLDS 25th anniversary, Linux and Windows (Wine) | dedicated, bots | — |
| Steam Half-Life 25th anniversary, Linux | listen server | scripted: listen server, joins HL25 and ReHLDS Linux servers |

Not validated yet: native Windows (only Wine so far), the Steam client on Windows, real keyboard
and mouse (every client run is scripted), joysticks, voice chat with microphones, long sessions
with many players and real latency, save games and level transitions, drowning and time-based
damage in-engine. Details: [docs/validation.md](docs/validation.md#32-not-validated); open issues:
[#1](https://github.com/the-specialists-mod/the-specialists-rs/issues/1) (real keyboard and mouse),
[#2](https://github.com/the-specialists-mod/the-specialists-rs/issues/2) (native Windows),
[#3](https://github.com/the-specialists-mod/the-specialists-rs/issues/3) (long sessions, latency),
[#4](https://github.com/the-specialists-mod/the-specialists-rs/issues/4) (remaining map features).

## Requirements

**To play or host:** a The Specialists 3.0 install (the maps, models, sounds and other content;
the port replaces only the game libraries) and one of these engines:

* Xash3D FWGS (Linux or Windows, 32- or 64-bit).
* Steam Half-Life (25th anniversary) on Linux or Windows, or Steam HLDS for a dedicated server.
* ReHLDS (Linux or Windows).

**To build:**

* Rust stable (edition 2024, so 1.85 or newer) with the four targets:
  `rustup target add i686-unknown-linux-gnu x86_64-unknown-linux-gnu i686-pc-windows-gnu x86_64-pc-windows-gnu`.
* `clang`/libclang, for bindgen (clang 18 or newer: clang 14 fails on the MinGW headers).
* MinGW-w64 (`i686-w64-mingw32-*` and `x86_64-w64-mingw32-*` gcc and binutils) for the Windows
  builds, and 32-bit glibc and libgcc for the i686 Linux build. Arch:
  `clang mingw-w64-gcc lib32-glibc lib32-gcc-libs`; Debian/Ubuntu: `libclang-dev mingw-w64 gcc-multilib`.
* An [xash3d-fwgs](https://github.com/FWGS/xash3d-fwgs) checkout next to this repository, with its
  `3rdparty/library_suffix` submodule (or set `FWGS_DIR`). The engine bindings are generated
  from its headers; the release workflow pins the commit it builds against.

**To run the checks** (`cargo xtask test`/`play`/`all`): Wine, the engine roots (set up with
steamcmd, ReHLDS and FWGS builds as described in [docs/validation.md](docs/validation.md#11-setting-the-roots-up)),
and TS content in `../ts_custom` (or `TS_DIR`).

## Quick start

Step-by-step instructions for each engine: [docs/INSTALL.md](docs/INSTALL.md).

Prebuilt libraries for all four platforms are on the
[releases page](https://github.com/the-specialists-mod/the-specialists-rs/releases): unpack the
archive for your platform and copy its `ts/` directory over your TS 3.0 install's `ts/`. The
`-debug` archives hold the same libraries with debug info, for crash reports.

To build them yourself:

```sh
cargo xtask build --release      # all four targets
cargo xtask dist --release       # dist/<platform>/ts/: libraries + liblist.gam, gameinfo.txt, delta.lst, icons
```

`--release` installs stripped, size-optimized libraries (about 1.2-1.8 MB each).
`--release-debug` installs the same build with its debug info (11-16 MB), which symbolizes crash
addresses from the stripped libraries with `addr2line -e`.

Pushing a `v*` tag builds both for all four platforms and publishes them as a GitHub release
(`.github/workflows/release.yml`): `the-specialists-rs-<tag>-<platform>.zip`/`.tar.gz` and the
matching `-debug` archives, each holding a `ts/` directory.

To play or host, copy `dist/<platform>/ts/` over a TS 3.0 install's `ts/` directory. Use the
`dist` output rather than raw `cargo build` DLLs: the xtask patches the Windows DLLs' export section
so Steam's engine can load them ([details](docs/validation.md#4-pitfalls-found-during-setup)).


| Platform | Server library | Client library |
|---|---|---|
| Linux, Steam HL / HLDS / ReHLDS | `dlls/ts_i386.so` and `dlls/ts.so` (stock HLDS loads the latter) | `cl_dlls/client.so` |
| Windows, Steam HL / HLDS / ReHLDS | `dlls/mp.dll` | `cl_dlls/client.dll` |
| Xash3D FWGS 64-bit | `dlls/ts_amd64.so` / `.dll` | `cl_dlls/client_amd64.so` / `.dll` |

Dedicated server, e.g. ReHLDS or HLDS on Linux:

```sh
./hlds_linux -game ts +maxplayers 16 +map ts_lobby
# bots: put `ts_bot_quota N` or `addbot` lines in ts/server.cfg (GoldSrc runs command-line
# +cvars before the game registers its cvars); `addbot` also works from the console and rcon.
```

## Development and validation

`cargo xtask` installs into local engine roots (linking the TS content without modifying it) and
runs checks:

```sh
cargo xtask roots                        # fwgs, fwgs-win, rehlds-linux, rehlds-win, hlds25-linux, hlds25-win
cargo xtask install all
cargo xtask server rehlds-linux --map ts_awaken --bots 4 --seconds 60
cargo xtask client hlds25-win            # FWGS client joins that server; screenshot in target/
cargo xtask test                         # server check on every root
```

Setting up the roots (steamcmd, ReHLDS, Wine) and the latest results: [docs/validation.md](docs/validation.md).

## Documentation

| Doc | Contents |
|---|---|
| [docs/INSTALL.md](docs/INSTALL.md) | Installing the port on Xash3D FWGS, Steam Half-Life and dedicated servers, bots, troubleshooting |
| [AGENTS.md](AGENTS.md) | Working in this repo (people and coding agents): crate layout, the `unsafe` boundary, build and test commands, conventions, pitfalls |
| [docs/design.md](docs/design.md) | Goals, the wire-compatibility contract, architecture (server, client, shared), module ownership, build targets, deviations from TS |
| [docs/validation.md](docs/validation.md) | Engine roots and how to set them up, test commands, results, setup pitfalls |
| [docs/issues/](docs/issues/README.md) | The original repository's 112 issues; issue numbers in the code and docs refer to these |
| [docs/implementation/movement.md](docs/implementation/movement.md) | Player movement and stunts |
| [docs/implementation/weapons.md](docs/implementation/weapons.md) | Weapons, combat, wire formats |
| [docs/implementation/rules.md](docs/implementation/rules.md) | Game rules, power-ups, TS entities, HUD sync, bots |
| [docs/implementation/stock-entities.md](docs/implementation/stock-entities.md) | Half-Life map entities used by TS maps |
| [docs/implementation/client-hud.md](docs/implementation/client-hud.md) | Client input, HUD, menus |
| [docs/implementation/client-rendering.md](docs/implementation/client-rendering.md) | Client view, events, effects, prediction |
| [docs/implementation/integration.md](docs/implementation/integration.md) | How the packages were joined, decisions, unused API, known gaps |
| [docs/implementation/plugin-api.md](docs/implementation/plugin-api.md) | The server plugin API (`CreateInterface`, for AMX Mod X's TS module) and the private-data guard |
| [docs/implementation/gameplay-plugins.md](docs/implementation/gameplay-plugins.md) | Gameplay rulesets as in-process plugins (`ts_gameplay`): the hooks, choosing the ruleset at a map start (a change reloads the map), the client check, adding a hook or a ruleset |
| [docs/implementation/ts21.md](docs/implementation/ts21.md) | The TS 2.1 gameplay plugin (`crates/ts21`, issue #100) |
| [docs/implementation/client-plugins.md](docs/implementation/client-plugins.md) | Client plugins, server-offered packages, and the player's approval with per-file hashes (built: packages, the store, start-up verification and prompt; not yet: server offers) |

## Origin

The workspace grew from [rust-hlmod](https://github.com/CrunchyMonkies/rust-hlmod), a minimal
GoldSrc mod in Rust. This repository keeps its history.

## License

Every crate in this workspace, including the server plugin API `ts-plugin-api`
(`crates/plugin-api`), is released under the [MIT License](LICENSE).
Copyright (c) 2026 The Specialists Mod contributors.

Third-party material and interfaces:

* The Specialists, Half-Life and their assets (maps, models, sounds, `.res`/`.txt` data) belong
  to their owners. They aren't in this repository and the MIT grant doesn't cover them.
* The engine and game ABI (struct layouts, export names, function tables) comes from the
  Half-Life SDK. `goldsrc-sys` generates its bindings at build time from an Xash3D FWGS checkout
  and doesn't commit them. Where code follows the SDK's own game code, that material is also
  subject to Valve's Half-Life 1 SDK license (non-commercial, for Half-Life products only).
* The entity-hook numbering in `crates/server/src/ham` (the `Ham_*` names and signature tags in
  `names.rs`) follows AMX Mod X's hamsandwich, so plugins written for it keep their numbers.
  These are interface identifiers. No AMX Mod X code is included.
* The AMX Mod X modules (`tsmod-amxx-rs`) and the RCBot port (`tsmod-rcbot-rs`) build on GPL
  code. They are separate repositories under the GPL, and they link `ts-plugin-api` under MIT,
  which is compatible with the GPL.
