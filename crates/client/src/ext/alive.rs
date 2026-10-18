//! Which other players are dead, for `visible_players` (issue #306, docs/implementation/
//! client-ext-api.md §6). Port addition: TS's client keeps no such thing. The server sends no
//! other player's health or `deadflag` (`game/ts/delta.lst` has them in the client data only), so
//! the client works it out from what it does get:
//!
//! * **Dead** from a `DeathMsg` naming the player as the victim (the kill feed; a blocked or held
//!   one counts too, as the HUD applies its state whatever the extensions say).
//! * **Alive again** when its entity comes in a packet newer than the death with a living
//!   movetype. A dead player's is `MOVETYPE_TOSS` (`CBasePlayer::Killed`) and then
//!   `MOVETYPE_NONE` once it lands (`PlayerDeathThink`, 0x8eaec); `CBasePlayer::Spawn` sets
//!   `MOVETYPE_WALK`. "Newer than the death" matters because the `DeathMsg` is read before the
//!   packet's entities: until they are, the entity still has the movetype it had alive.
//! * Also alive again (or no longer the question) on a `Spectator` message for the player (it
//!   became an observer, which `visible_players` skips, or stopped being one, a respawn), and for
//!   everyone on `InitHUD` (a new map) or a server change.
//!
//! The rejected alternative, TS's death sequences on the entity, depends on each player model's
//! sequence numbers.
//!
//! The body TS leaves when the player becomes an observer is a separate `deadcorpse` entity
//! (`TSCreateDeadCorpse`, 0x70234), never in slots 1..maxClients, so it isn't a candidate anyway.

use goldsrc::sys::{MOVETYPE_NONE, MOVETYPE_TOSS};

/// The dead players (by slot, 1..=32) and the packet each died after.
#[derive(Clone, Debug)]
pub struct Deaths {
    /// `Some(packet)` while dead: the local player's `messagenum` when the `DeathMsg` came.
    dead: [Option<i32>; 33],
}

impl Default for Deaths {
    fn default() -> Self {
        Deaths { dead: [None; 33] }
    }
}

/// A movetype a dead player's entity has.
pub fn dead_movetype(movetype: i32) -> bool {
    movetype == MOVETYPE_TOSS as i32 || movetype == MOVETYPE_NONE as i32
}

impl Deaths {
    /// A `DeathMsg` for `victim`; `packet` is the newest packet the client has read whole.
    pub fn died(&mut self, victim: i32, packet: i32) {
        if let Some(d) = self.dead.get_mut(victim as usize).filter(|_| victim > 0) {
            *d = Some(packet);
        }
    }

    /// A `Spectator` message for `slot`: whatever it says, the death is settled.
    pub fn spectator(&mut self, slot: i32) {
        if let Some(d) = self.dead.get_mut(slot as usize) {
            *d = None;
        }
    }

    /// A new map or server.
    pub fn clear(&mut self) {
        self.dead = [None; 33];
    }

    /// Whether `slot` is dead, its entity last seen in packet `messagenum` with `movetype`; a
    /// living movetype in a newer packet than the death ends it.
    pub fn is_dead(&mut self, slot: i32, messagenum: i32, movetype: i32) -> bool {
        let Some(d) = self.dead.get_mut(slot as usize) else { return false };
        match *d {
            Some(packet) if messagenum > packet && !dead_movetype(movetype) => {
                *d = None;
                false
            }
            Some(_) => true,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goldsrc::sys::MOVETYPE_WALK;

    const WALK: i32 = MOVETYPE_WALK as i32;
    const TOSS: i32 = MOVETYPE_TOSS as i32;
    const NONE: i32 = MOVETYPE_NONE as i32;

    #[test]
    fn a_victim_is_dead_until_its_entity_walks_again() {
        let mut d = Deaths::default();
        assert!(!d.is_dead(3, 10, WALK));
        // The DeathMsg of packet 11 is read while packet 10's entities are the newest.
        d.died(3, 10);
        assert!(d.is_dead(3, 10, WALK), "the entities of the death's packet aren't read yet");
        assert!(d.is_dead(3, 11, TOSS), "falling");
        assert!(d.is_dead(3, 40, NONE), "landed");
        assert!(!d.is_dead(3, 90, WALK), "respawned");
        assert!(!d.is_dead(3, 91, NONE), "and stays alive until the next death");
        assert!(!d.is_dead(4, 11, TOSS), "only the victim");
    }

    #[test]
    fn spectator_new_map_and_bad_slots() {
        let mut d = Deaths::default();
        d.died(5, 1);
        d.spectator(5);
        assert!(!d.is_dead(5, 1, NONE), "a Spectator message settles it");
        d.died(5, 1);
        d.died(6, 1);
        d.clear();
        assert!(!d.is_dead(5, 1, NONE) && !d.is_dead(6, 1, NONE));
        d.died(0, 1);
        d.died(-1, 1);
        d.died(200, 1);
        assert!(!d.is_dead(0, 0, NONE) && !d.is_dead(200, 0, NONE));
    }
}
