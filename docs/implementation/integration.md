# Integration

The six work packages (movement, weapons, stock entities, rules, client HUD, client rendering)
were built in parallel against the skeleton APIs in `docs/design.md`. This page records how they
were joined, what was decided along the way, and what is still open.

## 1. Wiring between packages

| Area | What was wired | Where |
|---|---|---|
| Protocol | Server HUD encoders (`protocol::hud_wire`, rules) and client decoders (`protocol::hud`, HUD) are separate modules. `protocol::wire_agreement` pins them to the same layout for the 13 messages both use. | `crates/shared/src/protocol/` |
| Effect messages | The engine keeps one hook per message. The HUD hooks all 64 and forwards `TSPSys`, `PTakeDam`, `TSFrags`, `TSMBlur`, `TSBTime` and `TSFade` raw to the render modules' `on_*` parsers. The render modules no longer register their own hooks. | `client/src/hud/mod.rs` |
| Client shared state | One per-player `active_items`/`laser_color` table (HUD writes, lasers read; indexed by entity number). The local predicted item bits are `local_items`. The spectator flags are filled by the HUD sync. | `client/src/state.rs` |
| Fall damage | Applied once, in the stunt PostThink, together with the landing logic. The rules copy was removed. | `server/src/stunt/moves.rs` |
| Death | `rules::killed` sets `DEAD_LOCK`, fills the stunt animation inputs (last damage, hitgroup, attack direction) and plays `CTSAnimation` 7. | `server/src/rules/lifecycle.rs` |
| Bullet dodge | Damage reads `p.stunt.dodge_until` (stunt sets it). | `server/src/player/damage.rs` |
| Bullet dance | Damage arms it (`stunt::anim::enable_`/`disable_bullet_dance`) on every hit to a living player and calls `stunt::anim::bullet_dance` when a dead one is shot. | `server/src/player/damage.rs` |
| Death animation | `death_think` keeps the player dying until `p.stunt.anim.seq_finished`, advancing the sequence with `stunt::anim::studio_frame_advance`. | `server/src/rules/lifecycle.rs` |
| Weapons → animation | Deploy and holster set the anim extension. Fire, reload, draw, grenade and melee call `stunt::animate`. | `server/src/weapons/` |
| Melee | One delayed-hit window (the gun's). Stunt hands open-window hits to weapons, which handles blocks, disarm and friendly fire. The super-jump roll landing sets the 0.75 s attack delay. | `server/src/weapons/`, `stunt/` |
| Kill scoring | A kill made during the killer's own weapon frame (the killer's object is taken out of the slab) used to score nothing. `world::defer` now queues the work until the object is back. | `server/src/world.rs`, `rules/scoring.rs` |
| Client prediction | `HUD_PostRunCmd` runs the shared `Gun` through a client `GunHost`, unpacked from `clientdata`/`weapon_data`. It plays the shooter's own fire events and calls `view::kick` once per predicted shot. Melee predicts a miss; the server decides hits. | `client/src/prediction/gun.rs` |
| HUD after spawn | `init_hud` clears the server's change-detection caches when it sends `ResetHUD`, so the values the client just zeroed are sent again. Clip, reserve and fire mode come from prediction when it matches the current weapon, because TS only sends `WeaponInfo` on weapon or reserve changes. | `rules/lifecycle.rs`, `client/src/hud/weapons.rs` |
| Third person | `view::frame` consumes the HUD's toggle, leave (observer) and restore requests. | `client/src/view.rs` |
| `+use` | One `PlayerUse` (`player::player_use`) picks a single object of any package (stock entities, ground weapons and ammo, dropped guns, knives, grenades), run from the weapons' PostThink inside the `ItemPostFrame` gate as in TS. The power-up double tap is the client's (`cl_usepowerup` sends `usepowerup`): TS's server-side `TSUse` (0x956e0) is never called. | `server/src/player/player_use.rs`, `weapons/mod.rs` |
| Spawn | Clears `FL_FROZEN` and resets the view (`trigger_camera`). | `server/src/player/mod.rs` |
| Cvars and commands | One owner each. Rules owns the shared weapon cvars (`realbullet`, `ammocount`, `weaponstay`, `damagemult`, `customweapons`) and `weaponrestriction`; weapons reads them by name and owns `loadweapons`/`saveweapons`. The two restriction masks are one. Cvars the engine already has (`sv_alltalk`) aren't re-registered; the check is `Engine::cvar_lookup`, which asks FWGS without its "non-existent cvar" warning (#331). | `rules/cvars.rs`, `weapons/` |
| Bots | A random loadout every spawn, reloading, weapon switching, aim gating, and chasing the nearest enemy. Only bots with a live user id are driven, because HL25 drops all bots when the last human leaves and `pfnRunPlayerMove` on a dropped edict crashes the engine. | `server/src/bots/` |
| Misc | `pfnResetGlobalState` resets `env_global`. `fire_targets` skips entities removed this frame. Water sounds are precached. `GetGameDescription` returns the per-mode TS string. `_special` is accepted silently. | `server-dll/src/lib.rs`, `entities/mod.rs`, `commands.rs` |

## 2. Decisions

* **Two shared-random implementations.** The TS client and server generators differ: the client
  keeps the random range as a float, while the server truncates it to an integer. That gives
  different pellet angles for the same seed. Both are kept (`shared::weapons::random` for the
  server, `client/src/events/random.rs` for the client's event playback), with a test that shows
  where they agree and where they differ. This matches TS, where the original client and server
  disagree the same way.
* **Development aid.** `ts_dev_scriptinput 1` (only with `sv_cheats 1`) lets scripted `+attack`
  etc. through the client's anti-script gate, for automated tests.
* **Dead code.** Both crates keep a crate-level `#![allow(dead_code)]` (see §4).

## 3. Test evidence (integration branch)

* `cargo test`: all pass. Builds for host, `i686-unknown-linux-gnu` and `i686-pc-windows-gnu`
  have no warnings. The Windows linker's stdcall note is handled by `--enable-stdcall-fixup`, and
  the export stays undecorated.
* `cargo xtask server fwgs --map ts_awaken --bots 4 --seconds 90` passes, and the bots fight.
  The A2S_PLAYER scores after 90 s were: Bot Neo 3, Bot Trinity 0, Bot Morpheus 1, Bot Niobe 0.
  Kill counts vary between runs (0–7).
* Listen server: predicted AK and pistol firing show the shooter's own muzzle flash and fire
  event. The server fires every predicted shot. After the `ResetHUD` fix, the HUD shows health,
  free slots, the weapon grid, the weapon info and the crosshair.

## 4. Unused API (why `dead_code` is allowed)

These items have no caller yet. They're kept because they're part of a TS feature that is
switched off in 3.0, or an SDK constant kept for completeness:

| Group | Items |
|---|---|
| Round and objective modes (dead in TS 3.0) | `rules::state::Mode::{RoundTdm, Objective}`, `send_award`, `CASH_FOR_WIN`/`CASH_FOR_LOSE`, `PlayerRules::cash`, `weapons::assets::{set_round_mode_items, set_grenade_buys}` |
| Weapons API for later features | `weapons::{drop_weapon, set_restriction}`, `glows_for`/`knife_glows_for`/`glow_for_host` (used by `AddToFullPack`), `Gun::{current, anim_extension}`, `shared_random_long` |
| Stunt helpers | `in_airborne_stunt` (damage uses the `STUNTING` mask), `block_player` |
| Entity helpers | `entities::use_targets`, `stock::base::{precache_model, precache_sound}`, `is_locked_by_master`, `clear_master`, `reset_player_room`, the `SF_*`/`FCAP_*`/`FFADE_*` constants |
| Messages | `messages::{pas, one_unreliable}` |
| SDK constants | unused `DMG_*` bits and hitgroups in `damage.rs`, `NearestPath` |
| Client | unused HUD cvar fields (`drawhistory_time`, `classautokill`, …), `hud::text::lookup`, `DIGITS_4`, `weapon_menu_consumed_attack`, `is_fwgs`, `effects::bytes`, `state::TsEffect::MotionBlur` (the HUD forwards `TSMBlur` raw) |

## 5. Known gaps after integration

* Recoil was checked against the decomp: the client `VKickback` (`0x10008230`), `V_ApplyKick`
  (`0x10057ed0`) and `ViewShake` (`0x10057fa0`/`0x10058000`) match the port's constants, and
  each shot permanently raises the view by `view_kick·0.85` (yaw by weapon class; ×0.775
  silenced, ×2/3 with double fire rate). Nothing pulls it back (`TSKickAlign` is unused), so the
  steep climb under sustained fire is TS behaviour. The server's `VKickback` (`0x80414`) uses the
  same constants (`.rodata` `0xf7f04..0xf7f20`) and writes `punchangle` for bots only. The
  client's melee attacks also start `ViewShake(amp, 0.2, 10)`; the port runs them through
  `GunHost::view_shake` (see `client-rendering.md`). A side-by-side check of the feel is still
  open.
* Weapon events (issues #11, #82): the anim choices, attachment sources, branch gates and sound
  volumes in `events::fire`/`events::misc` were re-traced to the client's disassembly. The
  view-model anim indices were re-checked (akimbo `[3..5]`/`[22..23]`/
  `[24..25]`, pump `[12]`/`[11]`, melee from set A, grenade body 1), and player attachments now
  come from TS's per-frame cache (`0x10056be0`), as TS reads them. In first person that cache
  holds the view model's muzzle on both engines (the view model's events pass copies its
  attachments into the local player before the cache is filled); spectating in-eye it holds the
  target's origin, where the port uses the view model's attachments instead
  (`client-rendering.md`, "Event parameters"). The predicted events now carry the player's origin
  and view angles (`HUD_PlaybackEvent` `0x1001ad50`); they were played with zeros, so the
  shooter's own bullets, impacts and decals went to the world origin and the flash pointed along
  world +x. The
  close-combat event plays TS's set-A index (#83) and traces 35/45/-55 by attack type (#85).
* The TS studio renderer (issue #2) and motion-blur ghosts (issue #3) are fully ported and were
  audited slot by slot against the asm (`client-rendering.md`); the view-model slow-motion rate
  now lives in the renderer as in TS. They are unit-tested but not yet checked in an engine
  (torso/legs split while running and aiming, corpses, ghosts during slow motion).
* Not tested with real keyboard and mouse. The only Steam GoldSrc client run so far is the
  scripted HL25 Linux listen server (`docs/validation.md` §5).

## 6. Deviations: engine calls that stop the server

Port rule: the game never crashes, hangs or stops the engine, even where TS does. TS makes some
engine calls that the Steam engines (HLDS, ReHLDS) answer with `Host_Error` (the map ends and the
server stops) or `Sys_Error` (the process quits), and a few that stop FWGS too. The wrappers in
`goldsrc` (`server.rs`, `ext/`) check those calls first, with the bookkeeping in `goldsrc::guard`
(unit-tested), and every refusal is logged with `at_error` and "(port guard)". Where FWGS only warns
(detected by its `host_ver` cvar, `Engine::is_xash`), the call is made as TS makes it, so FWGS
behaves exactly as with TS. Otherwise the port does what FWGS does with the same call:

| Call | Engine effect | Port |
|---|---|---|
| `PRECACHE_MODEL/SOUND/GENERIC`, `PRECACHE_EVENT` with an empty name or a first byte below `' '` (signed) | Steam: `Host_Error` "Bad string" (FWGS returns 0) | Not called; returns 0. Reached by a respawned `cycler_weapon` (stock-entities.md §2). |
| `PRECACHE_SOUND("!…")` | Steam: `Host_Error` "do not precache sentence names" (FWGS warns, 0) | Not called; returns 0. |
| A precache of a new name after `ServerActivate` | Steam: `Host_Error` "Precache can only be done in spawn functions" (FWGS late-precaches) | Not called; returns 0. A name already precached this level, in any case, is passed in its precached spelling (ReHLDS with `REHLDS_FIXES` compares case-sensitively). |
| A precache past 512 models, 512 sounds, 512 generic files or 256 events | Steam: `Host_Error` (FWGS's limits are higher) | Not called; returns 0. The tables fill in order, so the highest index the engine returned shows how full one is. |
| `PRECACHE_MODEL` of a model file that doesn't exist (a map's custom model), `PRECACHE_EVENT` of a missing script | Steam: `Sys_Error` "Mod_ForName: %s not found" (the model is loaded with `crash` set, optional `!` models too) / `Host_Error` "file missing from server" (FWGS warns) | Not called (the file is looked up with `pfnGetFileSize` first); returns 0. |
| `SET_MODEL`/`MODEL_INDEX`/`PRECACHE_MODEL` of a brush model `*n` the map doesn't have (only `*1` up to the BSP's model count minus 1, spelt `"*%i"`, are in the table) | Steam: `Host_Error` "no precache" (`SET_MODEL`), `Sys_Error` in `SV_ModelIndex`, `Sys_Error` "Mod_ForName: *n not found" (a precache while loading) or `Host_Error` "not precached" (after); FWGS registers a model with no data, which it then `Host_Error`s over for a `SOLID_BSP` entity ("SOLID_BSP with a non bsp model") | Guarded on every engine: `SET_MODEL` leaves the entity unchanged, `MODEL_INDEX` and the precache give 0. The count is the models lump's `filelen / 64`, read once per level from the header of `maps/<mapname>.bsp` (`pfnLoadFileForMe`, `Engine::brush_model_ok`); a BSP that isn't version 29/30 (FWGS's BSP2) lets every `*n` through. A `*n` the map has is passed on without the precache bookkeeping (it is already in the engine's table, also after `ServerActivate`). |
| `SET_MODEL`/`MODEL_INDEX` of a model not precached this level (not empty, not `*n`, not the world) | Steam: `Host_Error` "no precache" / ReHLDS `Sys_Error` in `SV_ModelIndex` (FWGS late-precaches, or warns) | `SET_MODEL` leaves the entity unchanged; `MODEL_INDEX` gives 0. |
| `EMIT_SOUND_DYN` with volume outside 0..1, attenuation outside 0..4, channel outside 0..7 or pitch outside 0..255 | ReHLDS: `Sys_Error` in `PF_sound_I` (FWGS clamps) | Clamped, as `SV_BuildSoundMsg` (and FWGS) does with the value it then gets; NaN is passed on, as the engines' checks pass it. |
| `SET_SIZE` with a min above its max | Steam: `Host_Error` "backwards mins/maxs" (FWGS logs, relinks) | Size kept, entity relinked (`SET_ORIGIN`). |
| `LIGHT_STYLE` outside 0..63 | FWGS: `Host_Error`; Steam writes outside `lightstyles[64]` | Skipped. |
| `SET_VIEW` on an entity outside the client slots | Steam: `Host_Error` "not a client" (FWGS logs) | Skipped. |
| `CREATE_ENTITY`/`CREATE_NAMED_ENTITY` with no edict left | Every engine: `ED_Alloc` "no free edicts" (`Sys_Error`/`Host_Error`) | Null, which every caller already handles (the entity isn't created). The wrapper reads the engine's edict array: free when the last slot was never used or a non-client slot is free and reusable (`ED_Alloc`'s 0.5 s rule). |
| A user message: `MSG_ONE` to a non-client or with no entity, type 0 | Steam: `Host_Error`/`Sys_Error` | Dropped. `MsgWriter` holds the writes until `end()` and checks the message before `MESSAGE_BEGIN`. |
| A variable-size user message over 192 bytes, a fixed-size one of the wrong size, any message over 512 bytes | Steam: `Host_Error` "Refusing to send user message" / `Sys_Error` (FWGS drops it) | Dropped (sizes as the Steam engines count them: a coord is a short). |
| A broadcast message with a target entity | Steam: `Sys_Error` | Sent without the entity. |
| `WRITE_ENTITY` of a number outside the edict range | FWGS: `Host_Error` (Steam writes the short) | Written with `WRITE_SHORT`: the same bytes. |

Not guarded:
* Engine-side failures the game doesn't call: a map over the edict limit while it loads, a
  BSP that can't be loaded, a reliable channel overflowing a client (that client is dropped; the
  server goes on).
* An entity whose `solid`, `movetype` or `modelindex` game code changes to one the physics
  stops on without relinking it (stock-entities.md §7.8 catches it at the next link).
* On FWGS the guards that only exist for the Steam engines are off, so FWGS's own limits (4096
  models, 2048 sounds, ...) apply.

### 6.1 The client

The client engine has the same kind of calls. Steam GoldSrc's client isn't open source; what it
does was read from the HL25 Linux `hw.so`, which ships with symbols (FWGS from its source). The
wrappers in `goldsrc` (`client.rs`, `ext/`) check these first, with the pure checks in
`goldsrc::cl_guard` (unit-tested); a refusal is printed to the console once per kind and name
("(port guard)"), not every frame. Steam-only checks use `ClEngine::is_xash` (FWGS's `host_ver`
cvar). Valid input reaches the engine unchanged.

| Call | Engine effect | Port |
|---|---|---|
| `SPR_Load` of a new name with the 256 HUD sprite slots taken | Steam: `Sys_Error` "cannot allocate more than 256 HUD sprites" (FWGS returns 0) | Steam: returns 0 without the call. The slots are tracked by the handles `SPR_Load` returned (`cl_guard::HudSprites`); the engine empties them on disconnect, which the port can't see, so it errs towards allowing. TS loads far fewer. |
| `PlaySoundByName` / `EV_PlaySound` with a name over 63 bytes | Steam: `Sys_Error` "Sound name too long" in `S_FindName` (FWGS ignores it) | Dropped on every engine (FWGS's result). |
| TriAPI `SpriteTexture` with a null model (a sprite that didn't load) | Steam: `R_TriangleSpriteTexture` reads `model->cache` through null (FWGS returns 0) | Returns false without the call. Reached by the overview's sprites when one is missing. |
| TriAPI `SpriteTexture` with a frame below 0 or past the sprite's last (2.1's unclamped particle frame, #270) | Steam: `R_GetSpriteFrame` (`hw.so` 0x143420) binds frame 0 (developer "Sprite: no such frame"); FWGS binds the last frame (frame 0 for a negative one), and with a frameless sprite reads before its table | Every engine is asked for frame 0 (Steam's answer); a sprite with no frames binds nothing (`tri::sprite_frame`). |
| `LoadMapSprite` of an image name not ending in `bmp`/`tga` (from the map's `overviews/*.txt`) | Steam: `Sys_Error` "Unsupported map image file format" | Steam: null without the call (the overview then has no map image). |
| Studio `GetModelByIndex` / `R_TempModel` with a negative model index | Steam: `CL_GetModelByIndex` only checks the upper bound, reads before `cl.model_precache` (FWGS returns null) | `None` without the call (`spectator::model_by_index` already refused 0 and below). |
| Studio `StudioSetupModel(bodypart)` with `bodypart == numbodyparts` or negative, a body part with `base` or `nummodels` 0, or an entity `body` that makes the submodel index negative | Both engines: only `bodypart > numbodyparts` is replaced, so the rest reads out of bounds; `body / base % nummodels` is a C integer division (`SIGFPE` on 0) | Skipped (`setup_model` returns false) and the renderer doesn't draw that body part. Checked against the header last given to `StudioSetHeader`; every TS model passes. |
| Studio `LoadCacheFile` of a missing or empty sequence group file (`<model>NN.mdl`) | FWGS: `Host_Error` "LoadCacheFile: can't load" (Steam leaves the cache slot empty) | FWGS: the file is read first (`COM_LoadFile`); missing or empty, the group has no animation (as on Steam). |
| `PlaybackEvent` with index 0 or (Steam) 256 and up | Steam: `CL_FireEvents` reads `cl.event_precache[index]` unchecked (FWGS checks the index against its own table) | Not called (index 0 plays nothing on either engine). |
| `R_DecalShoot` on an entity index the engine doesn't have | Steam: indexes `cl_entities` unchecked | Not called unless `GetEntityByIndex` (bounds-checked) has the entity. |
| `HookUserMsg` of a new name past 197 hooks | FWGS: `Host_Error` "MAX_USER_MESSAGES hit" (Steam keeps a list) | FWGS: not hooked. TS hooks far fewer. |

Checked and not fatal, so not guarded: `SPR_Set`/`SPR_Draw*`/`SPR_Width`/`SPR_Height`/`SPR_Frames`
with a bad handle (both engines check it; an unset sprite draws nothing) or frame (both clamp and
log), `SetCrosshair` (checked when drawn), `ClientCmd` (`Cbuf_AddText` prints "overflow"),
`ServerCmd` (Steam formats `cmd %s` into 2 KB; an overflowing net message is flagged and dropped,
as on FWGS), `ConsolePrint`/`CenterPrint` (HL25 `Con_Printf` uses a 4 KB `vsnprintf`,
`SCR_CenterPrint` copies at most 1023 bytes; FWGS likewise), registering a cvar or command twice
(both print; Steam's `pfnRegisterVariable` then returns an unlinked copy, and the port registers
each cvar once anyway, see `goldsrc::client::Cvar`), `HookEvent` past the engine's limit (ignored),
`GetEntityByIndex`, and a null `CL_TempEntAlloc*`/`R_TempModel`/`CL_AllocDlight` (the wrappers
return `Option`).

Not guarded:
* `Draw_DecalIndex` of a decal slot with no name: Steam `Sys_Error`s ("Used decal #%d without a
  name"). `Draw_DecalIndexFromName` answers 0 both for decal 0 and for an unknown name, and the
  API can't read the names, so the port can't tell; it needs a server that sent no decal list.
* `Mod_Extradata` of a model whose file was removed after it loaded (Steam `Sys_Error` "caching
  failed"), and the Steam `S_FindName` limit of 1024 distinct sounds per level.
* `EV_PlaySound` with `SND_SENTENCE` and a sentence number past the table (Steam reads out of
  bounds): the client never passes that flag.
