# AGENTS.md

Guidance for coding agents (and people) working in this repository: a Rust port of **The
Specialists 3.0** (TS), a GoldSrc / Half-Life mod. Both game libraries are ported: the server
(`mp.dll`, `ts_i386.so`) and the client (`client.dll`). The port is **wire-compatible with TS
3.0**, so the original TS client can join this server and this client speaks the same protocol.

Start with [README.md](README.md), then [docs/design.md](docs/design.md) (architecture and the
wire contract). Per-subsystem notes are in [docs/implementation/](docs/implementation/), and
testing is in [docs/validation.md](docs/validation.md).

## Sources of truth

| Question | Where to look |
|---|---|
| How TS behaves (numbers, timings, payloads, rules) | The decompilation, [the-specialists-decomp](https://github.com/the-specialists-mod/the-specialists-decomp), checked out at `../ts30-decomp`. Start at its `docs/README.md`; server addresses use the Ghidra numbering described there. |
| What the engine does | `../xash3d-fwgs` (FWGS source; the bindings are generated from its headers), `../ReHLDS` (Steam dedicated server reimplementation), `../halflife-sdk` (Valve's C++ game code). |
| How this port is organised | `docs/design.md`, `docs/implementation/*.md`. |
| Known gaps | GitHub issues labelled `integration` (and `validation` for untested platforms). Issue numbers in code, commits and docs written before the move to this repository (`#23`, `issue #62`) are the original repository's: see `docs/issues/<number>.md`. |

Port behaviour from the decomp, not from memory of the game. When the port deliberately differs
from TS, say so in the relevant `docs/implementation/*.md` "Deviations" section.

## Crate layout and the `unsafe` boundary

```
crates/
  goldsrc-sys/   bindgen output from the FWGS headers (generated per target)
  goldsrc/       safe wrappers over the engine: server/client function tables, edicts
                 (edict.rs), player-move callbacks (pmove.rs), native mouse (platform.rs), ...
  gameplay/      ts-gameplay: gameplay-plugin ids and registry (docs/implementation/gameplay-plugins.md)
  shared/        game logic both sides run: movement, weapons, protocol, TS constants
  server/        ts-server: server game logic (rlib)
  server-dll/    ts-server-dll: the server library the engine loads (cdylib "ts")
  plugin-api/    ts-plugin-api: the C ABI table server plugins get from CreateInterface (metamod
                 plugins: the AMX Mod X modules) or from the server's `tsmod_*` loader (bot
                 backends). no_std, versioned: a break bumps VERSION and the interface name
                 (docs/implementation/plugin-api.md, bot-plugins.md)
  tsmod-test/    tsmod_test, the stub `tsmod_*` plugin (a test fixture for `cargo xtask plugins`;
                 FFI from end to end, so it may use `unsafe` like the *-dll crates; never in dist/)
  plugin-pkg/    client plugin packages (.tspkg), manifests, signatures, the approval store and
                 per-file verification (docs/implementation/client-plugins.md §10). tsmod-launcher
                 links it too: no engine or port-crate dependencies, documented public API
  client-ext-api/ ts-client-ext-api: the C ABI the client offers client extensions (cl_dlls/tsmod_*
                 libraries, loaded only through the approval gate). no_std; VERSION exact, MINOR
                 fills reserved slots (docs/implementation/client-ext-api.md)
  tsmod-client-test/ the stub client extension (`cargo xtask client-plugins`); FFI like tsmod-test
  fake-launcher/ ts-launcher-fake, a stand-in for tsmod-launcher's relaunch side (IPC endpoint,
                 `relaunch` subcommand) for `cargo xtask relaunch`; a test fixture like
                 tsmod-test (its Windows named pipe is kernel32 FFI); never in dist/
  client/        ts-client: client game logic (rlib)
  client-dll/    ts-client-dll: the client library the engine loads (cdylib "client")
  ts30/          the TS 3.0 gameplay plugin: ts30-gameplay (shared hooks, the weapon tables in
                 data/), server/ (ts30-server: server and native-bot hooks) and client/
                 (ts30-client). Every TS 3.0 rule decision behind a hook lives here; only the
                 two *-dll crates link it (feature `ts30`, registered first: the default ruleset)
  ts21/          the TS 2.1 gameplay plugin: ts21-gameplay (shared hooks), server/ (ts21-server)
                 and client/ (ts21-client). All 2.1 code goes here; only the two *-dll crates
                 link it (feature `ts21`) (docs/implementation/ts21.md)
                 The hook traits have no default bodies, and `cargo xtask lint` checks that no
                 crate but its own names a ruleset outside test code
                 (docs/implementation/gameplay-plugins.md)
xtask/           build, package, install and test tooling (`cargo xtask ...`)
game/ts/         liblist.gam, gameinfo.txt, delta.lst, the icons (game.tga, ts_logo_big.tga)
```

**`unsafe` code lives only in `goldsrc`, `server-dll` and `client-dll`** (and the test fixtures
`tsmod-test` and `tsmod-client-test`, plugin libraries that are nothing but FFI, and `fake-launcher`). `ts-server`,
`ts-client`, `shared`, `ts-gameplay` and the `ts30-*`/`ts21-*` packages are `#![deny(unsafe_code)]`,
and `plugin-pkg` is `#![forbid(unsafe_code)]`. The rules:

1. **Never add `unsafe` to a game crate, and never weaken the lint.** The only `allow`s are the
   `goldsrc::export_entity_class!` macro (`#[no_mangle]` is an unsafe attribute in Rust 2024) and
   `shared`'s player-movement test module, which fakes the engine with `extern "C"` callbacks.
2. **Need new engine access? Add a safe wrapper to `goldsrc`.** Put it on `Engine`/`ClEngine`,
   the `EdictPtr` or `PlayerMoveExt` traits, or a module under `goldsrc/src/ext/` (e.g.
   `ext::tri` for the triangle API). Then call it from game code. Don't use
   `eng!`/`cl_eng!`/`ev!`/`efx!`/`tri!` in game crates: they expand to `unsafe` at the call site,
   and `deny(unsafe_code)` doesn't fire on another crate's macro expansion. A source check in
   `xtask/src/lint.rs` enforces this: it runs as a unit test (`cargo test`) and at the start of
   `cargo xtask build`, and lists the offending lines. Each game crate's `clippy.toml` also lists
   the macros under `disallowed-macros`, which `cargo xtask lint` turns into errors.
3. **A new engine callback or export goes in the `*-dll` crate.** Keep those functions thin:
   convert raw pointers to references (null-check), call a safe function in the game crate, and
   convert the result back. Logic belongs in the game crate. The one exception is
   `client-dll/src/tempents.rs`, the SDK temp-entity loop that walks the engine's linked list.
4. **Every `unsafe` block gets a `// SAFETY:` comment** saying why it holds. `goldsrc` and the
   `*-dll` crates warn on `clippy::undocumented_unsafe_blocks`; `cargo xtask lint` runs clippy
   with warnings as errors for all four targets, which matters because Windows-only code
   (`goldsrc::platform::win`, the WinMM joystick) is never compiled by a Linux clippy run.
5. **Entity pointers:** game code passes `*mut edict_t` around as a handle (holding a raw
   pointer is safe). Read and write through `goldsrc::edict::EdictPtr`: `e.v()` for `entvars_t`,
   `e.is_free()`, `e.has_private_data()`. `v()` returns `&'static mut entvars_t`, so keep that
   borrow short, and never hold it across an engine call that can re-enter the game for the same
   edict (touch, remove, damage). See the module docs in `goldsrc/src/edict.rs` and the take-out
   dispatch rules in `server/src/world.rs`.
6. **Hooks:** console commands are safe `extern "C" fn()`. User messages take `fn(&[u8])` and
   client events take `fn(&event_args_s)`; `goldsrc` provides the trampolines.

These wrappers make the unsafe code small and auditable. They can't make engine memory
provably sound, because the engine shares and mutates it, so respect the borrow rules above.

## Build and test

Requirements: Rust stable, `clang` (bindgen), MinGW-w64 for the Windows targets, Wine for the
Windows engine roots, and `../xash3d-fwgs` (or set `FWGS_DIR`).

```sh
cargo test                                # unit tests (all crates)
cargo xtask build [--release]             # all four targets; must build with no warnings
cargo xtask lint                          # clippy --workspace --all-targets -D warnings, all four targets
                                          # (~40 s cold), plus the engine-macro check
cargo xtask install <root>|all            # assemble <root>/ts and install the libraries
cargo xtask all                           # test + play (meck and bots included), about 4.5 min; then steam, only when
                                          # Steam is already running and logged in (skipped with a note otherwise)
cargo xtask test [--map M]                # dedicated server + bots on every engine root, in parallel (~25 s)
cargo xtask play [fwgs fwgs-win]          # listen-server gameplay on ts_dojo: join/buy/spawn, wall shots, lasers,
                                          # akimbo, power-up, grenade, kill, kick, death/respawn, bot fights, addbot,
                                          # Escape; then meck and bots; both roots in parallel (~4 min)
cargo xtask meck [fwgs fwgs-win]          # grenades on MecklenburgV_Rain_Final: a grenade kill and broken func_breakables
cargo xtask bots [fwgs fwgs-win]          # TS's bot AI on ts_dojo: testpath, four addbot bots roaming the node graph
                                          # and fighting, botlearn
cargo xtask plugins [<root>...]           # the tsmod_* loader (stub plugin + fixtures it must skip) and the stub's
                                          # bot backend (ts_bot_backend test) on a dedicated server, all roots
cargo xtask client-plugins [fwgs fwgs-win] # the client extension loader (stub + fixtures it must skip), the
                                          # extension API (TSExt both ways with the server stub plugin, the
                                          # interception hooks) and plugin approval (packages, tampering, the
                                          # start-up prompt's hook) on a listen server; dev profile only
                                          # (about a minute per root, longer under Wine)
cargo xtask plugin-approve <path>... [--root R]
                                          # approve a local client plugin build (.tspkg, cl_plugins/<id>/<ver>,
                                          # cl_dlls/tsmod_<name><suffix>) in cl_plugins/approved.toml
cargo xtask ts21 [--no-clients] [<root>...]
                                          # TS 2.1 mode on every root's dedicated server, driven by rcon:
                                          # the server config's and rcon's ts_gameplay reloads, LMS, The One,
                                          # teamplay, round TDM, objective rounds, a large map; then the port's
                                          # and the original client on the Linux Steam roots (~6 min, not in all)
cargo xtask version [<root>...]           # the port client's version check at connect (sv_client_version,
                                          # sv_client_require_port) on each Linux root's dedicated server: the
                                          # FWGS port client refused and admitted, the original client refused
                                          # (~1.5 min per root, not in all)
cargo xtask relaunch [--xvfb] [fwgs fwgs-win]
                                          # restart and reconnect end to end (e2e client build +
                                          # ts-launcher-fake): IPC, relaunch arguments, self-relaunch,
                                          # quit and inform, next start, passworded server; kills the
                                          # root's games it started, so run it alone (~5 min per root,
                                          # not in all; docs/validation.md §2.r)
cargo xtask tspkg [<root>...]             # server offers of client plugins on each Linux root's dedicated server:
                                          # plugin_offers.toml and the package checks, TSPkg to the FWGS port client,
                                          # tspkg replies, the required-offer kick, op 5; nothing to clients that
                                          # don't hook TSPkg; then the client's offer flow with the e2e client and a
                                          # FastDL stand-in: cards, downloads, tampered packages, an update, op 5's
                                          # card through ts-launcher-fake; enforcement: a [[block]] suspending (resumed
                                          # on leaving) and kicking, a required match with op 3 and the
                                          # sv_plugin_download_time kick, unlisted; the plugins panel's revoke, unblock
                                          # and delete (not in all; Xvfb without a desktop)
cargo xtask plugin-offer [--required] <tspkg>
                                          # check a built package, write its .manifest.toml/.sig/.cert siblings,
                                          # print its plugin_offers.toml entry
cargo xtask client <server-root>          # FWGS client joins a dedicated server; screenshot in target/
cargo xtask steam [listen|<server-root>]  # the Steam HL25 client (Half-Life/ts): play's scenes + changelevel + the
                                          # Steam-only regressions on a listen server (~7 min), or joining
                                          # hlds25-linux / rehlds-linux (~4 min), then the root in 2.1 mode (~30 s);
                                          # starts Steam if it can log in alone;
                                          # friends status invisible meanwhile (docs/validation.md §5)
cargo xtask dist --release                # dist/<platform>/ts/: libraries + game files (stripped)
cargo xtask dist --release-debug          # the same release build, libraries with debug info
cargo xtask package-xash-win              # windows-i686 dist --release + stock TS 3.0 + FWGS win32
                                          # -> ~/ts-xash-win-release.tar.gz (writes to $HOME)
```

TS content: dev and test commands link `TS_DIR`, default `../ts_custom` (with the custom maps,
e.g. `MecklenburgV_Rain_Final`; found from the checkout's parents, so worktrees work).
`../ts_original` (stock TS 3.0, `TS_ORIGINAL_DIR`) and `../xash3d-ts` (`XASH_WIN_DIR`) are only for
`package-xash-win`; `TS_PACKAGE_OUT` overrides the archive path.

Engine roots (`cargo xtask roots`) default to `~/games/<dir>`: `fwgs` (native amd64 FWGS),
`fwgs-win` (FWGS 0.21 win32 under Wine), `fwgs-win64` (FWGS win32-amd64 under Wine), `rehlds-linux`, `rehlds-win`, `hlds25-linux` and
`hlds25-win`. Setup steps are in `docs/validation.md` §1.

Before calling a change done:
- `cargo fmt` is applied (`rustfmt.toml`: 140 columns, `use_small_heuristics = "Max"`), so
  `cargo fmt --check` is clean.
- `cargo test` passes.
- `cargo xtask build` has no warnings.
- `cargo xtask lint` passes: the workspace is clippy-clean on all four targets. Fix a lint
  rather than silence it; where it fights a deliberate TS- or SDK-faithful pattern, use a
  narrowly scoped `#[allow(clippy::...)]` with a comment saying why, never a crate-wide one.
- Run `cargo xtask all` (or `test` alone for server-only changes that can't affect gameplay).
- When the change touches exports or the `*-dll` crates, check that the exported symbol lists are
  unchanged (`nm -D --defined-only` / `objdump -p`).

Scripted in-engine tests use dev aids that need `sv_cheats 1`:
- `ts_dev_scriptinput 1` lets scripted `+attack` through the client's anti-script gate.
- `ts_dev_touch <file>` writes the end-of-script marker.
- `ts_tp` teleports, and `ts_face` aims a player's view.
- `ts_bot_stop 1` freezes living bots (dead ones still buy and respawn); plugin bots too (`run_cmd`).
- `ts_bot_backend native|<name>` picks the bot backend; like `ts_gameplay`, a change reloads the
  map (docs/implementation/bot-plugins.md, gameplay-plugins.md §4). Bots run TS's AI
  (`crates/server/src/bots`, decomp `docs/server/bots.md`). Deviation (user's choice): `addbot`,
  `addcustombot` and `ts_bot_quota` bots run on dedicated servers too (server console, configs
  and rcon only); TS has bots only on listen servers.
- `ts_observe` sets an observer's mode and target; `ts_dev_fxlog 1` (client) logs each predicted
  shot's flash, bullet, impact and decal, akimbo shots and laser beams as `[ts fx]` lines.
- `ts_dev_keylog 1` (client) logs Escape's `HUD_Key_Event` answer and `cancelselect` as `[ts key]`.
- `ts_as <player#> <command> ...` runs a client command as another player (e.g. a bot's `ts_give`).
- `ts_dev_hamlog 1` logs the plugin entity hooks (Spawn, TakeDamage, Killed) as `[ts ham]` lines
  (`docs/implementation/plugin-api.md` §7).
- `ts_dev_key <key> <1|0>` (client) presses a key through the engine's `Key_Event`;
  `ts_dev_extsuspend <name> <1|0>` suspends a client extension (both need `ts_dev_scriptinput 1`).
  Client extensions load only if approved (`cl_plugins/approved.toml`, the start-up prompt, or
  `cargo xtask plugin-approve`); debug builds also honour `TSRS_DEV_CLIENT_EXT=<names>`
  (docs/implementation/client-ext-api.md §3) and `TSRS_DEV_PLUGIN_PROMPT=<id>=<answer>,...`, which
  answers the start-up boxes (client-plugins.md §7.1). Release builds ignore both. `--features e2e`
  client builds (only `cargo xtask relaunch` and `tspkg` build them) answer the relaunch and offer
  cards and the `cl_plugins_menu` panel from `TSRS_E2E_CARDS=<file>` (client-plugins.md §11).
  Nothing approves from the console.
  `ts_dev_extlog 1` (client) logs the extensions' interception decisions as `[ts ext]` lines
  (client-ext-api.md §11).
- `ts_dev_tspkg 1` (server) counts every port client as hooking `TSPkg`: builds between releases
  report the last release's version, below `TSPKG_SINCE` (docs/implementation/client-plugins.md §12).
- `ts_status`, `ts_hurt` and `ts_give` are also available.

## Conventions

- **Wire compatibility is a hard constraint.** Don't rename, reorder or resize user messages.
  Don't change event names or parameter meanings, `delta.lst` field usage, or TS client commands.
  `shared::protocol` holds the table, and tests pin the encoders to the decoders.
- **Cite the decomp.** Doc comments name the TS function and address they port, e.g.
  ``/// `CBasePlayer::StopObserver` (0x99a14)``. Keep doing that.
- **Formatting** is rustfmt's, with the repo `rustfmt.toml`. Don't hand-format against it.
- **Comments say why**, not what. Doc comments go on public items.
- **Docs move with the code.** Update `docs/design.md` (architecture), the matching
  `docs/implementation/*.md` (behaviour, deviations, gaps) and `docs/validation.md` (tests,
  pitfalls) in the same change.
- **Port-only additions are marked.** Cvars, commands and dev aids that TS doesn't have
  (`ts_bot_quota`, `ts_tp`, …) are commented as port additions.

## Pitfalls (learned the hard way)

- **GoldSrc runs command-line `+cvar` before `GameInit` registers the game's cvars.** Set game
  cvars from a cfg via `+servercfgfile` (xtask does).
- **The game queues `exec <servercfgfile>` (`lservercfgfile` on a listen server) on every map**,
  as TS's `CHalfLifeMultiplay` constructor does. The command waits behind a running `wait` script,
  and TS's shipped `listenserver.cfg` adds four bots and sets `sv_cheats 0`, so xtask's listen-server
  scripts set `lservercfgfile ""`.
- **Stock HLDS strips `_i386` and loads `dlls/ts.so`**, so xtask installs both names.
- **Windows DLLs need MSVC-style export sections.** Steam's engine finds exports through the
  section named `.rdata`, so xtask renames MinGW's `.edata`. Use `cargo xtask install`/`dist`, not
  raw `cargo build` DLLs.
- **Don't trust `pfnGetPlayerUserId` for liveness.** After a client drops, FWGS still maps its
  edict to the freed slot (old user id, NULL `cl->edict`). `ClientDisconnect` clears
  `FL_CLIENT`/`FL_FAKECLIENT`, which `players()`/`is_player()` rely on. TS keeps the flag, so ports
  of TS player tests use `rules::has_player`/`ts_flags`/`slot_players`, never the flag.
- **HL25 drops every bot when the last human leaves.**
- **Movement and weapon floating point is per TS build** (`shared::pm::Arith`, `movement.md`, `weapons.md` §5).
  The client predicts as `client.dll` (`Msvc`); the server runs `mp.dll`'s identical arithmetic on
  every platform unless the port's cvar `ts_pm_arith gcc` selects `ts_i386.so`'s. Server code that
  uses the movement maths or the shared weapon maths takes `rules::cvars::pm_arith()` (the weapon
  code through `GunHost::arith`), never a fixed `Arith`.
- **Server commands the game queues run late in scripted tests.** `pfnServerCommand` text (e.g.
  the bot quota's `kick`) goes to the end of the console buffer, behind a running `wait` script.
  End the script; don't `quit` from it.
- **The roots link the content's `maps` directory.** Never run `savenodes` (or anything that
  writes `maps/`) from a scripted test: it would overwrite `../ts_custom/maps`.
- **The FWGS engine log is buffered under Wine**, so don't wait for a line in it while the game
  runs. Scripts end with `ts_dev_touch` and xtask polls for that file.
- **FWGS's console buffer is 32 KB.** Long `wait` chains overflow it and the whole cfg is skipped
  (`Cbuf_InsertTextToBuffer: overflow`). xtask scripts use `w10`/`w100` aliases.
- **Windows crash dialogs (`Sys_Crash`) name the nearest export**, which is meaningless. Solve
  the DLL's load base from the frame addresses and the export RVAs, then
  `addr2line -e mp.dll` on the debug build. For a crash in a shipped (stripped) release, use
  the `--release-debug` libraries of the same commit: same code, with debug info.
- **Stopping engines:** use `pkill -x xash` (or `xash3d`). `pkill -f` patterns can match your
  own shell.

## Licensing

The workspace is MIT (`LICENSE`, `license = "MIT"` in `[workspace.package]`; every crate sets
`license.workspace = true`, so a new crate must too). New code is MIT unless it derives from GPL
code. Don't copy or translate code from GPL sources (Xash3D FWGS engine code, AMX Mod X,
metamod, RCBot, ReHLDS, ReGameDLL) into this repository. Describing their behaviour, matching
an interface (names, numbers, struct layouts) and porting from TS's own binaries and the
Half-Life SDK are fine. If GPL-derived code is genuinely needed, it belongs in a separate GPL
crate or repository (like `tsmod-amxx-rs`), not in an MIT crate. Ask first, and mark it in the
crate's `license` field and the README's License section.

## Git

- Work on `main` unless told otherwise. Commit or push only when asked.
- If git has no HTTPS credentials, push with
  `git -c credential.helper='!gh auth git-credential' push`.
- Releases are numbered after the TS version they port (`v3.0.0`, then `v3.0.1`, ...). To make one:
  1. In one commit named "Release vX.Y.Z": bump `version` in the workspace `Cargo.toml`
     (`[workspace.package]`) to the new number, with the `Cargo.lock` a build then writes, and
     update the README's latest-release link and its "vX.Y.Z passed it ... N unit tests" line
     (run `cargo xtask all --release` first).
  2. Tag that commit `vX.Y.Z` and push the tag.

  **The tag must be `v` + the `Cargo.toml` version.** The libraries report that version (the
  server browser's "The Specialists v3.0.2 (DM)" / "(DM-21)", the HUD's `SrvSett` keys, the
  client's `_tsrs` userinfo), and `.github/workflows/release.yml` fails a tag that doesn't match.
  The workflow then builds and publishes the release and `-debug` archives. It pins the FWGS
  commit the bindings are generated from.
  Each asset is named `the-specialists-rs-<tag>-<platform>.zip` (Windows) or `.tar.gz` (Linux),
  plus the `-debug` variants, for `linux-i686`, `linux-x86_64`, `windows-i686` and
  `windows-x86_64`. The launcher picks the latest release's asset by that ending, so keep these
  names even though the repository is now `the-specialistsmod-rs`. The workflow's "Check asset
  names" step fails the release if they change.
- Track gaps and bugs as GitHub issues (`gh issue create -R the-specialists-mod/the-specialistsmod-rs`).
  Integration work uses the `integration` label.

## Lodging what you find

Every bug, gap or TS mismatch you find gets a GitHub issue, whether or not you fix it:

- **Found and fixed in your change:** lodge an issue for it too, so there's a record of what was
  wrong, and close it from the commit (`Fixes #N`). This includes problems found along the way,
  not just the one you were asked to fix.
- **Found but out of scope:** don't fix it in passing and don't leave it only in a report or a
  code comment. Lodge an issue.
- **Verify first.** Confirm it against the decomp, a test or a log before filing. Give the
  evidence: TS addresses, the port's `file:line`, and what happens versus what TS does.
- Combine closely related findings into one issue. Search the open issues first
  (`gh issue list --search ...`) and comment on an existing one instead of filing a duplicate.
- Label it: `bug` for wrong behaviour, `enhancement` for missing features, `integration` for
  engine integration, `validation` for untested platforms, `documentation` for docs.
- Something that turns out not to be a bug: close it with a comment that explains why.
