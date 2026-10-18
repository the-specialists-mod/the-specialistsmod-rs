# Installing The Specialists 3.0 (Rust port)

The port replaces only TS's game libraries: the server (`dlls/`) and the client (`cl_dlls/`), plus
`liblist.gam`, `gameinfo.txt`, `delta.lst` and two icons (`game.tga`, `ts_logo_big.tga`). Maps, models, sounds and everything else come from a
normal The Specialists 3.0 install. You need:

1. **An engine:** Xash3D FWGS, Steam Half-Life, or a dedicated server (Steam HLDS or ReHLDS). See
   "Which engine" below.
2. **The TS 3.0 content:** the stock `ts/` directory.
3. **The port's libraries:** a release archive for your platform, or your own build.

On Xash3D FWGS the [TS launcher](https://github.com/the-specialists-mod/ts-installer) does all three
steps for you (`ts-launcher install --root DIR --xash`). The rest of this page is the manual route.

## 1. Which engine

| Engine | Platform | Archive to use | Status |
|---|---|---|---|
| Xash3D FWGS, 64-bit | Linux, Windows | `linux-x86_64` / `windows-x86_64` | Tested (Linux natively; Windows not yet on real hardware) |
| Xash3D FWGS 0.21, 32-bit | Windows | `windows-i686` | Tested under Wine |
| Steam Half-Life (25th anniversary) | Linux | `linux-i686` | Tested: listen server, joining dedicated servers |
| Steam Half-Life (25th anniversary) | Windows | `windows-i686` | Not tested yet |
| Steam HLDS (25th anniversary) or ReHLDS | Linux, Windows | `linux-i686` / `windows-i686` | Tested: dedicated servers with bots |

The pre-anniversary Steam client (build 8684) isn't supported. Pre-anniversary HLDS works through
ReHLDS. Details of what has and hasn't been validated: [validation.md](validation.md#32-not-validated).

## 2. Get the TS 3.0 content

TS 3.0 ships as `ts-3-final.exe` (ModDB, also mirrored at
`https://archive.org/download/ts-3.0/ts-3-final.exe`, MD5 `e0fe976a8d9d59393e66b9f1cb94c256`).
Install it, or unpack it, and keep its `ts/` directory: that is the "TS content" below. An
existing TS 3.0 install works just as well.

## 3. Get the port's libraries

Download the archive for your platform from the
[releases page](https://github.com/the-specialists-mod/the-specialists-rs/releases):
`the-specialists-rs-<version>-<platform>.zip` (Windows) or `.tar.gz` (Linux). Each holds a `ts/`
directory:

| Platform | Server library | Client library |
|---|---|---|
| `linux-i686` (Steam HL, HLDS, ReHLDS) | `dlls/ts_i386.so` and `dlls/ts.so` | `cl_dlls/client.so` |
| `windows-i686` (Steam HL, HLDS, ReHLDS, FWGS 0.21 win32) | `dlls/mp.dll` | `cl_dlls/client.dll` |
| `linux-x86_64` (FWGS 64-bit) | `dlls/ts_amd64.so` | `cl_dlls/client_amd64.so` |
| `windows-x86_64` (FWGS 64-bit) | `dlls/mp_amd64.dll` | `cl_dlls/client_amd64.dll` |

The `-debug` archives hold the same libraries with debug info. Use them only to symbolize crash
reports.

To build them yourself instead, see the Requirements in the [README](../README.md#requirements),
then run `cargo xtask dist --release`. The output is in `dist/<platform>/ts/`. Use `dist`, not raw
`cargo build` DLLs: xtask fixes the Windows DLLs' export section, without which Steam's engine
crashes while loading them.

## 4. Install

In every case: put the TS content in the engine's `ts/` directory, then copy the archive's `ts/`
over it, replacing the files with the same names. TS's own `mp.dll`, `client.dll` and
`liblist.gam` are overwritten; keep a copy of the content if you want to switch back to the
original game.

### Xash3D FWGS

1. Get FWGS from [its releases](https://github.com/FWGS/xash3d-fwgs/releases), matching the
   archive you chose (64-bit, or the 0.21 win32 build for `windows-i686`).
2. FWGS needs Half-Life's `valve/` directory next to it, from a Steam Half-Life install or another
   source you're entitled to use.
3. Copy the TS content into `ts/` next to `valve/`, then copy the archive's `ts/` over it.
4. Start it with `xash3d -game ts` (Windows: `xash3d.exe -game ts`).

The layout:

```
xash3d(.exe)  + its libraries
valve/        Half-Life base files
ts/           TS 3.0 content, with the port's dlls/, cl_dlls/, liblist.gam, gameinfo.txt, delta.lst, icons
```

### Steam Half-Life

1. Install Half-Life (app 70) in Steam.
2. Copy the TS content into `Half-Life/ts` (Linux: `~/.local/share/Steam/steamapps/common/Half-Life/ts`;
   Windows: `C:\Program Files (x86)\Steam\steamapps\common\Half-Life\ts`), then copy the
   archive's `ts/` over it (`linux-i686` on Linux, `windows-i686` on Windows).
3. Restart Steam. The mod is listed as "The Specialists (Rust)" with the TS icon
   (`liblist.gam`'s `icon "ts_logo"` makes Steam load `ts_logo_big.tga`; `game.tga` is the
   window icon). You can also start Half-Life with the launch option `-game ts`.

### Dedicated server (HLDS or ReHLDS)

1. Install HLDS with steamcmd (app 90). For the 25th-anniversary build use `-beta public`; for
   ReHLDS install the `steam_legacy` branch and put the ReHLDS binaries over it:

   ```sh
   ./steamcmd.sh +force_install_dir ~/hlds +login anonymous \
       +app_set_config 90 mod valve +app_update 90 -beta public validate +quit
   ```

   On Linux the Steam API also needs `~/.steam/sdk32/steamclient.so`:
   `mkdir -p ~/.steam/sdk32 && ln -sf ~/hlds/steamclient.so ~/.steam/sdk32/steamclient.so`.
2. Copy the TS content into `hlds/ts`, then copy the archive's `ts/` over it (`linux-i686` or
   `windows-i686`).
3. Start the server:

   ```sh
   ./hlds_run -game ts +maxplayers 16 +map ts_lobby        # Linux
   hlds.exe -console -game ts +maxplayers 16 +map ts_lobby # Windows
   ```

GoldSrc runs command-line `+cvar`s before the game registers its cvars, so put game settings
(`mp_timelimit`, `lastmanstanding`, ...) in `ts/server.cfg` rather than on the command line.

## 5. Bots

* `addbot [model]` and `addcustombot [name] [model] [skill]` add TS's bots. Bots walk the map's
  node graph (`maps/<map>.ndm`); on a map without one, `botlearn 1` lets them build it as players
  move around, and `savenodes` writes it.
* **Listen server** (Create Server in the menu): as in the original game. TS's stock
  `listenserver.cfg` adds four.
* **Dedicated server:** TS has no bots there; the port allows them. `addbot` and
  `addcustombot` are server commands, so only the server's console or command line, its configs
  (`ts/server.cfg`) and `rcon` can run them, not players. The port also adds
  `ts_bot_quota N`, which keeps N bots running; put it in `ts/server.cfg`.

The port also adds `ts_pm_arith` (default `msvc`): player movement and the weapon maths
(timers, spread, kick) are computed as TS's Windows server computes them, on every platform, so
clients predict them exactly. `ts_pm_arith gcc` makes a server compute them as TS's Linux server
did instead.

`ts_gameplay` (default `30`, port addition) picks the gameplay ruleset: `30` is TS 3.0 and `21`
is TS 2.1, which is still being built. A map runs the ruleset it selects when it starts: set it in
`game.cfg`, the server config (`server.cfg`, or the file `+servercfgfile` names), at the console
or by rcon. Changing it to another ruleset reloads the current map under it at once (the console
says `[ts] gameplay: ruleset 21: reloading <map>`; players reconnect as at a map change), and so
does changing `ts_bot_backend` (the bot AI, default `native`). In 2.1 mode only this port's client can join: TS 3.0's own client is refused with
"Invalid Version: Running 2.1 mode. Upgrade to the rust port at tsmod.org". The buy menu shows
the ruleset and the client's and the server's versions.

Port additions for client versions (`implementation/client-plugins.md` §13.1). By default every
client joins.

* `sv_client_version` (default `any`): which versions of this port's client may join. `same_minor`
  (the server's `3.0`), `exact` (the server's own version), or a requirement such as
  `>=3.0.8, <3.1`. A refused player is told both versions, for example "This server needs The
  Specialists port 3.0.8 (yours: 3.0.7). Update via the TS launcher or tsmod.org. [tsrs:=3.0.8]".
* `sv_client_require_port 1` also refuses TS 3.0's own client.
* `sv_client_version_url` replaces `tsmod.org` in that text.

The first two show in the server browser's rules, so the launcher can warn before joining. This
is about compatibility, not cheating: a modified client can claim any version. A player can set
`cl_version_strict 1` to leave any server whose version isn't their client's.

## 6. Check that the port is running

* The mod is listed as "The Specialists (Rust)", and the console shows
  `[ts] GameInit: Rust TS server library loaded` when the server starts (the first map of a
  listen server, or the dedicated server's start).
* The original TS 3.0 client can join a server running the port, and the port's client can join
  an original TS 3.0 server: both speak the same protocol.

## 7. Uninstall

Copy TS's original `dlls/`, `cl_dlls/`, `liblist.gam`, `gameinfo.txt` and `delta.lst` back from
the TS 3.0 content (and delete `game.tga` and `ts_logo_big.tga`, which TS doesn't have), or
reinstall TS 3.0.

## Troubleshooting

| Problem | Fix |
|---|---|
| HLDS on Linux: `Couldn't get DLL API` | Stock HLDS loads `dlls/ts.so`, ReHLDS `dlls/ts_i386.so`. The `linux-i686` archive has both; make sure both were copied. |
| Steam engine on Windows crashes while loading `mp.dll` | The DLL wasn't built with `cargo xtask dist`/`install` (see step 3). Use a release archive or `dist/`. |
| HLDS on Linux: Steam API errors at start | Create the `~/.steam/sdk32/steamclient.so` link (Dedicated server, step 1). |
| Map loads, but there are no bots on a listen server | Run `addbot` in the console. On a map without `maps/<map>.ndm`, set `botlearn 1` and walk around first. |
| Port messages ending in "(port guard)" in the console | The port skipped an engine call that TS makes and that would stop the engine. Not an error; report it if it happens on a stock map. |

More setup details and known engine quirks: [validation.md](validation.md#4-pitfalls-found-during-setup).
