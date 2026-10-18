//! Client prediction glue: weapon prediction in HUD_PostRunCmd and the Txfer callbacks that carry
//! TS fields (iuser4 stunt bits, fuser1 time scale, vuser1..4 weapon state). Owner:
//! rendering/prediction work package. Reference: rendering-effects-prediction.md §8.

mod gun;

use crate::state;
use goldsrc::client::engine;
use goldsrc::sys::*;

pub use gun::{alt_grip, reset_alt_grip, reset_spread, vid_init};

/// TS keeps one weapon, the `CTSGun` container, in HL weapon slot 16.
pub const TS_GUN_SLOT: usize = 16;

/// `HUD_PostRunCmd` (`0x100045b0` -> `HUD_WeaponsPostThink` `0x10003830`).
pub fn post_run_cmd(from: &mut local_state_t, to: &mut local_state_t, cmd: &mut usercmd_t, runfuncs: bool, time: f64, seed: u32) {
    gun::post_think(from, to, cmd, runfuncs, time, seed);
    // The zoom target `CHud::Think` eases toward (`0x100df1a4`), on every call as in TS.
    state::with(|s| s.fov_target = to.client.fov);
    // Port addition (issue #306): the client extensions' `LocalState` ducking flag, from the
    // predicted state of the command just run (the last call of a frame is the newest).
    crate::ext::view::note_ducking(to.client.flags & FL_DUCKING as i32 != 0);
}

/// `HUD_TxferLocalOverrides` (`0x1001b900`): SDK set plus `iuser4`.
pub fn txfer_local_overrides(s: &mut entity_state_t, c: &clientdata_t) {
    s.origin = c.origin;
    s.iuser1 = c.iuser1;
    s.iuser2 = c.iuser2;
    s.iuser3 = c.iuser3;
    s.iuser4 = c.iuser4;
}

/// `HUD_ProcessPlayerState` (`0x1001b950`): the SDK field list plus `iuser4`; for the local
/// player the team number and the spectator globals.
pub fn process_player_state(dst: &mut entity_state_t, src: &entity_state_t) {
    dst.origin = src.origin;
    dst.angles = src.angles;
    dst.velocity = src.velocity;
    dst.frame = src.frame;
    dst.modelindex = src.modelindex;
    dst.skin = src.skin;
    dst.effects = src.effects;
    dst.weaponmodel = src.weaponmodel;
    dst.movetype = src.movetype;
    dst.sequence = src.sequence;
    dst.animtime = src.animtime;
    dst.solid = src.solid;
    dst.rendermode = src.rendermode;
    dst.renderamt = src.renderamt;
    dst.rendercolor = src.rendercolor;
    dst.renderfx = src.renderfx;
    dst.framerate = src.framerate;
    dst.body = src.body;
    dst.controller = src.controller;
    dst.blending = src.blending;
    dst.basevelocity = src.basevelocity;
    dst.friction = src.friction;
    dst.gravity = src.gravity;
    dst.gaitsequence = src.gaitsequence;
    dst.spectator = src.spectator;
    dst.usehull = src.usehull;
    dst.playerclass = src.playerclass;
    dst.team = src.team;
    dst.colormap = src.colormap;
    dst.iuser4 = src.iuser4;
    if engine().local_player().is_some_and(|p| p.index == dst.number) {
        state::with(|s| {
            s.player_class = dst.playerclass;
            s.team_number = dst.team;
            s.spec_mode = src.iuser1;
            s.spec_target = src.iuser2;
            s.spec_flags = src.iuser3;
        });
    }
}

/// `HUD_TxferPredictionData` (`0x1001bb00`): carries predicted-only fields across a new server
/// frame. `fuser1` needs no copy (pmove reads it from clientdata every command, never writes it).
pub fn txfer_prediction_data(
    ps: &mut entity_state_t,
    pps: &entity_state_t,
    pcd: &mut clientdata_t,
    ppcd: &clientdata_t,
    wd: &mut [weapon_data_t; 32],
    pwd: &[weapon_data_t; 32],
) {
    ps.oldbuttons = pps.oldbuttons;
    ps.flFallVelocity = pps.flFallVelocity;
    ps.iStepLeft = pps.iStepLeft;
    ps.playerclass = pps.playerclass;

    pcd.viewmodel = ppcd.viewmodel;
    pcd.m_iId = ppcd.m_iId;
    pcd.ammo_shells = ppcd.ammo_shells;
    pcd.ammo_nails = ppcd.ammo_nails;
    pcd.ammo_cells = ppcd.ammo_cells;
    pcd.ammo_rockets = ppcd.ammo_rockets;
    pcd.m_flNextAttack = ppcd.m_flNextAttack;
    pcd.fov = ppcd.fov;
    pcd.weaponanim = ppcd.weaponanim;
    pcd.tfstate = ppcd.tfstate;
    pcd.maxspeed = ppcd.maxspeed;
    pcd.deadflag = ppcd.deadflag;
    crate::input::set_alive(ppcd.iuser1, ppcd.deadflag);
    // Spectator: keep the server's values for spectate-only clients (SDK behaviour).
    if goldsrc::ext::spectator::is_spectate_only() {
        state::with(|s| {
            pcd.iuser1 = s.spec_mode;
            pcd.iuser2 = s.spec_target;
            pcd.iuser3 = s.spec_flags;
        });
    } else {
        pcd.iuser1 = ppcd.iuser1;
        pcd.iuser2 = ppcd.iuser2;
        pcd.iuser3 = ppcd.iuser3;
    }
    pcd.iuser4 = ppcd.iuser4;
    pcd.fuser2 = ppcd.fuser2;
    pcd.fuser3 = ppcd.fuser3;
    pcd.vuser1 = ppcd.vuser1;
    pcd.vuser2 = ppcd.vuser2;
    pcd.vuser3 = ppcd.vuser3;
    pcd.vuser4 = ppcd.vuser4;
    *wd = *pwd;
}
