//! Entity hooks for server plugins (`ts_plugin_api` version 2): a port addition, TS has no
//! plugin API. AMX Mod X's hamsandwich hooks TS's C++ virtual functions by patching vtables; the
//! port's entities are Rust objects without vtables (`world.rs`), so the server calls the
//! plugin's hook around each "virtual" it has, and runs one on request ([`execute`]).
//! docs/implementation/plugin-api.md §7 has the table of functions.
//!
//! # Where hooks run
//! The functions keep their place in the game; their callers go through this module instead of
//! calling `world::dispatch` (or the function) directly:
//! * **Outer** calls reach the object through its edict: [`spawn`], [`think`], [`touch`],
//!   [`use_`], [`take_damage`], ... The hook before runs while the object is still in its slot,
//!   then `world::dispatch` takes it out for the function, and the hook after runs once it is
//!   back. They fire only for an object in its slot: one a dispatch further up the stack holds is
//!   skipped by `world::dispatch` too, so there is no call to hook.
//! * **Inner** calls happen inside the object's own dispatch, on the `&mut` the caller holds
//!   (TS's `this->Jump()` from `PreThink`, `Killed` from `TakeDamage`): [`jump`], [`duck`],
//!   [`killed`], [`player_spawn`], [`respawn_item`]. Each hook call lends the object back to its
//!   slot and opens it (`world::lend_open`), so the hook reaches it as it does for outer calls.
//!
//! # Cost
//! Each entry point first tests one bit ([`Hooks::armed`]): set only while a hook is installed
//! and the plugin watches that function (`ham_watch`). Unwatched, the call is the plain one.
//!
//! # Re-entrancy
//! Hooks run with no `RefCell` borrow held and the object in its slot, so a hook may call the
//! API, run functions ([`execute`], which goes through these same entry points), deal damage, or
//! create and remove entities. A function that can no longer reach its object (removed by the
//! pre hook) doesn't run; the post hook still does. Hook calls nest at most [`MAX_DEPTH`] deep:
//! deeper, functions run without hooks and [`execute`] refuses with `BUSY`, so a plugin that
//! re-runs a function from its own hook (`ExecuteHamB(Ham_Spawn)` in a Spawn hook) stops instead
//! of overflowing the stack.

mod classes;
mod gun;
mod names;
mod query;
mod table;
#[cfg(test)]
mod tests;

pub use gun::{GunFrame, gun_item, gun_post, gun_pre, item_drop, item_pre_frame, item_update_client_data, send_weapon_anim};
pub use names::{NAMES, SIGS};
pub use query::{
    blood_color, can_use_through_walls, center, classify, enable_objective, eye_position, give_slow_mul, gun_position, in_slow, is_alive,
    is_bsp_model, is_in_world, is_net_client, is_objective, is_player, killed_entity, object_caps, on_free_private_data,
};

use crate::damage::DamageInfo;
use crate::player::Player;
use crate::world::{self, Cx, Entity, UseType};
use goldsrc::Global;
use goldsrc::edict::EdictPtr;
use goldsrc::server::engine;
use goldsrc::sys::*;
use std::cell::Cell;
use std::ffi::{CStr, c_int, c_void};
use ts_plugin_api::ham::{action, kind};
use ts_plugin_api::{BUSY, HamArg, HamCall, INVALID, NOT_PLAYER, OK, UNSUPPORTED};

/// The number of functions in AMX Mod X 1.10's `Ham` enum (`HAM_LAST_ENTRY_DONT_USE_ME_LOL`).
pub const COUNT: usize = NAMES.len();

/// hamsandwich's `Ham` numbers of the functions the port has (`ham_const.inc`).
pub mod id {
    pub const SPAWN: usize = 0;
    pub const KEYVALUE: usize = 2;
    pub const OBJECTCAPS: usize = 3;
    pub const ACTIVATE: usize = 4;
    pub const CLASSIFY: usize = 6;
    pub const TRACEATTACK: usize = 8;
    pub const TAKEDAMAGE: usize = 9;
    pub const TAKEHEALTH: usize = 10;
    pub const KILLED: usize = 11;
    pub const BLOODCOLOR: usize = 12;
    pub const OVERRIDERESET: usize = 25;
    pub const ISALIVE: usize = 32;
    pub const ISBSPMODEL: usize = 33;
    pub const ISINWORLD: usize = 36;
    pub const ISPLAYER: usize = 37;
    pub const ISNETCLIENT: usize = 38;
    pub const THINK: usize = 41;
    pub const TOUCH: usize = 42;
    pub const USE: usize = 43;
    pub const BLOCKED: usize = 44;
    pub const RESPAWN: usize = 45;
    pub const CENTER: usize = 48;
    pub const EYEPOSITION: usize = 49;
    pub const ITEM_CANDEPLOY: usize = 65;
    pub const ITEM_DEPLOY: usize = 66;
    pub const ITEM_CANHOLSTER: usize = 67;
    pub const ITEM_HOLSTER: usize = 68;
    pub const ITEM_UPDATEITEMINFO: usize = 69;
    pub const ITEM_PREFRAME: usize = 70;
    pub const ITEM_POSTFRAME: usize = 71;
    pub const ITEM_DROP: usize = 72;
    pub const ITEM_UPDATECLIENTDATA: usize = 77;
    pub const ITEM_GETWEAPONPTR: usize = 78;
    pub const ITEM_ITEMSLOT: usize = 79;
    pub const WEAPON_SENDWEAPONANIM: usize = 85;
    pub const WEAPON_PLAYEMPTYSOUND: usize = 83;
    pub const WEAPON_PRIMARYATTACK: usize = 87;
    pub const WEAPON_SECONDARYATTACK: usize = 88;
    pub const WEAPON_RELOAD: usize = 89;
    pub const WEAPON_WEAPONIDLE: usize = 90;
    pub const WEAPON_SHOULDWEAPONIDLE: usize = 92;
    pub const WEAPON_USEDECREMENT: usize = 93;
    pub const PLAYER_JUMP: usize = 55;
    pub const PLAYER_DUCK: usize = 56;
    pub const PLAYER_PRETHINK: usize = 57;
    pub const PLAYER_POSTTHINK: usize = 58;
    pub const PLAYER_GETGUNPOSITION: usize = 59;
    pub const TS_BREAKABLERESPAWN: usize = 94;
    pub const TS_CANUSEDTHROUGHWALLS: usize = 95;
    pub const TS_GIVESLOWMUL: usize = 123;
    pub const TS_GOSLOW: usize = 124;
    pub const TS_INSLOW: usize = 125;
    pub const TS_ISOBJECTIVE: usize = 126;
    pub const TS_ENABLEOBJECTIVE: usize = 127;
    pub const TS_ONFREEENTPRIVATEDATA: usize = 128;
    pub const TS_SHOULDCOLLIDE: usize = 129;
    pub const TS_WEAPON_ALTERNATEATTACK: usize = 418;
}

/// How many hook calls may be on the stack at once (see the module docs).
pub const MAX_DEPTH: u32 = 8;

/// The plugin's hook as the game calls it (`ts-server-dll` wraps the C function pointer): `post`
/// false before the function, true after. Returns a `ham::action`.
pub type HookFn = fn(call: &mut HamCall, post: bool) -> c_int;

// ---- marshalling --------------------------------------------------------------------------------

/// What marshalling and the hook calls need from the game, so tests can run without an engine.
pub(crate) trait Env {
    /// The entity index of `e`, -1 for NULL.
    fn index_of(&self, e: *mut edict_t) -> c_int;
    /// The edict of a game entity in use at `index` (not -1).
    fn edict_of(&self, index: c_int) -> Option<*mut edict_t>;
    /// `e`'s class ([`class_id`]), 0 if it has none.
    fn class_of(&self, e: *mut edict_t) -> c_int;
    fn error(&self, msg: &str);
}

/// One argument of a hooked function, in its native type.
pub(crate) trait ArgVal {
    const KIND: c_int;
    fn put(&mut self, env: &impl Env) -> HamArg;
    /// Takes the hook's change. Values the function can't use are ignored.
    fn take(&mut self, a: &HamArg, env: &impl Env);
}

fn arg(kind: c_int) -> HamArg {
    HamArg { kind, ..HamArg::default() }
}

impl ArgVal for i32 {
    const KIND: c_int = kind::INT;
    fn put(&mut self, _: &impl Env) -> HamArg {
        HamArg { i: *self, ..arg(Self::KIND) }
    }
    fn take(&mut self, a: &HamArg, _: &impl Env) {
        *self = a.i;
    }
}

/// Guard (port choice): a NaN or infinite damage, amount or direction from a plugin would spread
/// into health and velocities; it is ignored.
impl ArgVal for f32 {
    const KIND: c_int = kind::FLOAT;
    fn put(&mut self, _: &impl Env) -> HamArg {
        HamArg { f: *self, ..arg(Self::KIND) }
    }
    fn take(&mut self, a: &HamArg, _: &impl Env) {
        if a.f.is_finite() {
            *self = a.f;
        }
    }
}

impl ArgVal for [f32; 3] {
    const KIND: c_int = kind::VECTOR;
    fn put(&mut self, _: &impl Env) -> HamArg {
        HamArg { v: *self, ..arg(Self::KIND) }
    }
    fn take(&mut self, a: &HamArg, _: &impl Env) {
        if a.v.iter().all(|f| f.is_finite()) {
            *self = a.v;
        }
    }
}

/// An entity argument (hamsandwich's Entvar, Cbase, Edict): its index, -1 for NULL.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ent(pub *mut edict_t);

impl ArgVal for Ent {
    const KIND: c_int = kind::ENTITY;
    fn put(&mut self, env: &impl Env) -> HamArg {
        HamArg { i: env.index_of(self.0), ..arg(Self::KIND) }
    }
    fn take(&mut self, a: &HamArg, env: &impl Env) {
        if a.i == env.index_of(self.0) {
            return;
        }
        match a.i {
            -1 => self.0 = std::ptr::null_mut(),
            i => match env.edict_of(i) {
                Some(e) => self.0 = e,
                None => env.error(&format!("a hook passed entity {i}, which isn't in use; the argument is unchanged")),
            },
        }
    }
}

/// `Use`'s type: 0 off, 1 on, 2 set, 3 toggle.
impl ArgVal for UseType {
    const KIND: c_int = kind::INT;
    fn put(&mut self, _: &impl Env) -> HamArg {
        HamArg { i: *self as c_int, ..arg(Self::KIND) }
    }
    fn take(&mut self, a: &HamArg, _: &impl Env) {
        *self = match a.i {
            0 => UseType::Off,
            1 => UseType::On,
            2 => UseType::Set,
            3 => UseType::Toggle,
            _ => *self,
        };
    }
}

/// `TraceAttack`'s `TraceResult *`: the hook gets a pointer to this one and its writes land here.
impl ArgVal for TraceResult {
    const KIND: c_int = kind::TRACE;
    fn put(&mut self, _: &impl Env) -> HamArg {
        HamArg { p: std::ptr::from_mut(self).cast::<c_void>().cast_const(), ..arg(Self::KIND) }
    }
    fn take(&mut self, _: &HamArg, _: &impl Env) {}
}

/// `Ham_Keyvalue`'s `KeyValueData *`, passed as hamsandwich's int (`p`, and `i` its low 32 bits).
pub(crate) struct Kvd(pub KeyValueData);

impl ArgVal for Kvd {
    const KIND: c_int = kind::INT;
    fn put(&mut self, _: &impl Env) -> HamArg {
        let p = std::ptr::from_mut(&mut self.0).cast::<c_void>();
        HamArg { i: p as usize as c_int, p: p.cast_const(), ..arg(Self::KIND) }
    }
    fn take(&mut self, _: &HamArg, _: &impl Env) {}
}

/// A function's arguments, `args[..KINDS.len()]`.
pub(crate) trait Args {
    const KINDS: &'static [c_int];
    fn put(&mut self, out: &mut [HamArg; ts_plugin_api::ham::MAX_ARGS], env: &impl Env);
    fn take(&mut self, args: &[HamArg; ts_plugin_api::ham::MAX_ARGS], env: &impl Env);
}

macro_rules! tuple_args {
    ($($t:ident $n:tt),*) => {
        impl<$($t: ArgVal),*> Args for ($($t,)*) {
            const KINDS: &'static [c_int] = &[$($t::KIND),*];
            #[allow(unused_variables)]
            fn put(&mut self, out: &mut [HamArg; ts_plugin_api::ham::MAX_ARGS], env: &impl Env) {
                $(out[$n] = self.$n.put(env);)*
            }
            #[allow(unused_variables)]
            fn take(&mut self, args: &[HamArg; ts_plugin_api::ham::MAX_ARGS], env: &impl Env) {
                $(self.$n.take(&args[$n], env);)*
            }
        }
    };
}
tuple_args!();
tuple_args!(A 0);
tuple_args!(A 0, B 1);
tuple_args!(A 0, B 1, C 2);
tuple_args!(A 0, B 1, C 2, D 3);
tuple_args!(A 0, B 1, C 2, D 3, E 4);

/// A function's return value.
pub(crate) trait Ret: Sized {
    const RET: c_int;
    fn put(&self, env: &impl Env) -> HamArg;
    fn take(a: &HamArg, env: &impl Env) -> Self;
    fn zero() -> Self;
}

impl Ret for () {
    const RET: c_int = kind::VOID;
    fn put(&self, _: &impl Env) -> HamArg {
        arg(Self::RET)
    }
    fn take(_: &HamArg, _: &impl Env) -> Self {}
    fn zero() -> Self {}
}

impl Ret for bool {
    const RET: c_int = kind::INT;
    fn put(&self, _: &impl Env) -> HamArg {
        HamArg { i: *self as c_int, ..arg(Self::RET) }
    }
    fn take(a: &HamArg, _: &impl Env) -> Self {
        a.i != 0
    }
    fn zero() -> Self {
        false
    }
}

impl Ret for i32 {
    const RET: c_int = kind::INT;
    fn put(&self, _: &impl Env) -> HamArg {
        HamArg { i: *self, ..arg(Self::RET) }
    }
    fn take(a: &HamArg, _: &impl Env) -> Self {
        a.i
    }
    fn zero() -> Self {
        0
    }
}

impl Ret for f32 {
    const RET: c_int = kind::FLOAT;
    fn put(&self, _: &impl Env) -> HamArg {
        HamArg { f: *self, ..arg(Self::RET) }
    }
    fn take(a: &HamArg, _: &impl Env) -> Self {
        a.f
    }
    fn zero() -> Self {
        0.0
    }
}

impl Ret for [f32; 3] {
    const RET: c_int = kind::VECTOR;
    fn put(&self, _: &impl Env) -> HamArg {
        HamArg { v: *self, ..arg(Self::RET) }
    }
    fn take(a: &HamArg, _: &impl Env) -> Self {
        a.v
    }
    fn zero() -> Self {
        [0.0; 3]
    }
}

impl Ret for Ent {
    const RET: c_int = kind::ENTITY;
    fn put(&self, env: &impl Env) -> HamArg {
        HamArg { i: env.index_of(self.0), ..arg(Self::RET) }
    }
    fn take(a: &HamArg, env: &impl Env) -> Self {
        Ent(if a.i == -1 { std::ptr::null_mut() } else { env.edict_of(a.i).unwrap_or(std::ptr::null_mut()) })
    }
    fn zero() -> Self {
        Ent(std::ptr::null_mut())
    }
}

// ---- the hook calls -----------------------------------------------------------------------------

const WORDS: usize = COUNT.div_ceil(64);

/// The installed hook, the watched functions and the hook calls on the stack. The game has one
/// ([`HOOKS`]); tests make their own.
pub(crate) struct Hooks {
    hook: Cell<Option<HookFn>>,
    watched: [Cell<u64>; WORDS],
    /// `watched` while a hook is installed, else 0: the one test on the fast path.
    armed: [Cell<u64>; WORDS],
    depth: Cell<u32>,
}

/// A call whose hook ran before the function.
pub(crate) struct Frame {
    call: HamCall,
    pre: c_int,
}

impl Frame {
    pub(crate) fn superseded(&self) -> bool {
        self.pre == action::SUPERCEDE
    }
}

impl Hooks {
    pub(crate) const fn new() -> Self {
        Hooks {
            hook: Cell::new(None),
            watched: [const { Cell::new(0) }; WORDS],
            armed: [const { Cell::new(0) }; WORDS],
            depth: Cell::new(0),
        }
    }

    pub(crate) fn set_hook(&self, hook: Option<HookFn>) {
        self.hook.set(hook);
        self.rearm();
    }

    pub(crate) fn watch(&self, ham: usize, on: bool) {
        let w = &self.watched[ham / 64];
        let bit = 1u64 << (ham % 64);
        w.set(if on { w.get() | bit } else { w.get() & !bit });
        self.rearm();
    }

    fn rearm(&self) {
        let on = self.hook.get().is_some();
        for (a, w) in self.armed.iter().zip(&self.watched) {
            a.set(if on { w.get() } else { 0 });
        }
    }

    /// The hook runs for `ham`.
    #[inline(always)]
    pub(crate) fn armed(&self, ham: usize) -> bool {
        self.armed[ham / 64].get() & (1u64 << (ham % 64)) != 0
    }

    pub(crate) fn depth(&self) -> u32 {
        self.depth.get()
    }

    fn invoke(&self, hook: HookFn, call: &mut HamCall, post: bool, reach: &mut dyn FnMut(&mut dyn FnMut())) -> c_int {
        let mut act = action::CONTINUE;
        self.depth.set(self.depth.get() + 1);
        reach(&mut || act = hook(call, post));
        self.depth.set(self.depth.get() - 1);
        act
    }

    /// The hook before `ham` runs on `e` with `args`; their changes are taken. `None` when no hook
    /// runs (none installed, or nested [`MAX_DEPTH`] deep): run the function plainly then.
    /// `reach` runs the hook with `e`'s object reachable.
    pub(crate) fn pre<A: Args>(
        &self,
        env: &impl Env,
        ham: usize,
        e: *mut edict_t,
        ret_kind: c_int,
        args: &mut A,
        reach: &mut dyn FnMut(&mut dyn FnMut()),
    ) -> Option<Frame> {
        let hook = self.hook.get()?;
        if self.depth.get() >= MAX_DEPTH {
            env.error(&format!("hooks nested {MAX_DEPTH} deep; {} runs without them", NAMES[ham]));
            return None;
        }
        let mut call = HamCall {
            ham: ham as c_int,
            this: env.index_of(e),
            class: env.class_of(e),
            argc: A::KINDS.len() as c_int,
            ret: arg(ret_kind),
            orig_ret: arg(ret_kind),
            ..HamCall::default()
        };
        args.put(&mut call.args, env);
        let (this, class) = (call.this, call.class);
        let pre = self.invoke(hook, &mut call, false, reach);
        args.take(&call.args, env);
        // What the plugin may have scribbled on besides the arguments and `ret`.
        (call.ham, call.this, call.class, call.argc) = (ham as c_int, this, class, A::KINDS.len() as c_int);
        Some(Frame { call, pre })
    }

    /// The hook after the function, which returned `ran` (`None`: it didn't run). The post call's
    /// action decides the result: `OVERRIDE` gives the hook's `ret`, anything else what the
    /// function returned, or the zero value if the pre hook superseded it.
    pub(crate) fn post<A: Args, R: Ret>(
        &self,
        env: &impl Env,
        frame: Frame,
        ran: Option<R>,
        args: &mut A,
        reach: &mut dyn FnMut(&mut dyn FnMut()),
    ) -> Option<R> {
        let Frame { mut call, pre } = frame;
        // The arguments as the function saw them.
        args.put(&mut call.args, env);
        call.orig_ret = ran.as_ref().map_or(arg(R::RET), |r| r.put(env));
        if pre != action::OVERRIDE && pre != action::SUPERCEDE {
            call.ret = call.orig_ret;
        }
        call.ret.kind = R::RET;
        // The plugin may have removed its hook meanwhile (unloading): nothing to call then.
        let post = self.hook.get().map_or(action::CONTINUE, |hook| self.invoke(hook, &mut call, true, reach));
        if post == action::OVERRIDE {
            Some(R::take(&call.ret, env))
        } else if pre == action::SUPERCEDE {
            Some(R::zero())
        } else {
            ran
        }
    }

    /// Pre hook, the function (unless superseded), post hook, for a function that `reach` lets
    /// the hook reach without holding the object.
    pub(crate) fn call<A: Args, R: Ret>(
        &self,
        env: &impl Env,
        ham: usize,
        e: *mut edict_t,
        args: &mut A,
        reach: &mut dyn FnMut(&mut dyn FnMut()),
        run: impl FnOnce(&mut A) -> Option<R>,
    ) -> Option<R> {
        let Some(frame) = self.pre(env, ham, e, R::RET, args, reach) else { return run(args) };
        let ran = if frame.superseded() { None } else { run(args) };
        self.post(env, frame, ran, args, reach)
    }
}

static HOOKS: Global<Hooks> = Global(Hooks::new());

/// The engine and the slab.
struct Game;

impl Env for Game {
    fn index_of(&self, e: *mut edict_t) -> c_int {
        if e.is_null() { -1 } else { engine().index_of(e) }
    }
    fn edict_of(&self, index: c_int) -> Option<*mut edict_t> {
        game_entity(index)
    }
    fn class_of(&self, e: *mut edict_t) -> c_int {
        world::type_of(e).map_or(0, classes::class_of_type)
    }
    fn error(&self, msg: &str) {
        engine().print(&format!("[ts] ham error: {msg}\n"));
    }
}

/// The edict of game entity `index`: in range, in use, with the game's private data.
fn game_entity(index: c_int) -> Option<*mut edict_t> {
    let eng = engine();
    if index < 0 || index >= eng.globals().maxEntities {
        return None;
    }
    let e = eng.entity_of_index(index);
    (!e.is_null() && !e.is_free() && e.has_private_data()).then_some(e)
}

fn plain(f: &mut dyn FnMut()) {
    f()
}

/// An outer call through `world::dispatch`: hooks fire while the object is in its slot.
#[inline(always)]
fn outer<A: Args, R: Ret>(ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if !HOOKS.0.armed(ham) {
        return run(args);
    }
    outer_hooked(ham, e, args, run)
}

#[inline(never)]
fn outer_hooked<A: Args, R: Ret>(ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if !world::is_present(e) {
        return run(args);
    }
    HOOKS.0.call(&Game, ham, e, args, &mut plain, run)
}

/// An outer call that doesn't need the object taken out (it changes entvars, or reaches a lent
/// player as `world::run_now` does): hooks fire for an object in its slot or lent to it.
#[inline(always)]
fn outer_now<A: Args, R: Ret>(ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if !HOOKS.0.armed(ham) {
        return run(args);
    }
    outer_now_hooked(ham, e, args, run)
}

#[inline(never)]
fn outer_now_hooked<A: Args, R: Ret>(ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if world::type_of(e).is_none() || world::run_now(e, || ()).is_none() {
        return run(args);
    }
    let mut reach = |f: &mut dyn FnMut()| {
        if world::run_now(e, &mut *f).is_none() {
            f();
        }
    };
    HOOKS.0.call(&Game, ham, e, args, &mut reach, run)
}

/// An inner call on `obj`, the object of `cx.edict` that the caller's dispatch holds.
#[inline(always)]
fn inner<T: Entity + Default, A: Args, R: Ret>(
    ham: usize,
    obj: &mut T,
    cx: &mut Cx,
    mut args: A,
    run: impl FnOnce(&mut T, &mut Cx, &mut A) -> R,
) -> R {
    if !HOOKS.0.armed(ham) {
        return run(obj, cx, &mut args);
    }
    inner_hooked(ham, obj, cx, args, run)
}

#[inline(never)]
fn inner_hooked<T: Entity + Default, A: Args, R: Ret>(
    ham: usize,
    obj: &mut T,
    cx: &mut Cx,
    args: A,
    run: impl FnOnce(&mut T, &mut Cx, &mut A) -> R,
) -> R {
    let e = cx.edict;
    inner_on(ham, e, obj, cx, args, run)
}

/// An inner call of `ham` on `this` (the object of `cx.edict`, or another entity that belongs to
/// it: the player's `weapon_tsgun`), with `obj` lent to `cx.edict`'s slot for the hooks.
fn inner_on<T: Entity + Default, A: Args, R: Ret>(
    ham: usize,
    this: *mut edict_t,
    obj: &mut T,
    cx: &mut Cx,
    mut args: A,
    run: impl FnOnce(&mut T, &mut Cx, &mut A) -> R,
) -> R {
    let e = cx.edict;
    if !world::can_lend(e) {
        return run(obj, cx, &mut args);
    }
    let h = &HOOKS.0;
    let Some(frame) = h.pre(&Game, ham, this, R::RET, &mut args, &mut |f| world::lend_open(e, obj, f)) else {
        return run(obj, cx, &mut args);
    };
    let ran = if frame.superseded() { None } else { Some(run(obj, cx, &mut args)) };
    h.post(&Game, frame, ran, &mut args, &mut |f| world::lend_open(e, obj, f)).unwrap_or_else(R::zero)
}

// ---- the game's calls ---------------------------------------------------------------------------

/// `pEntity->Spawn()` (`DispatchSpawn` and the game's own spawns).
pub fn spawn(e: *mut edict_t) {
    outer(id::SPAWN, e, &mut (), |_| world::dispatch(e, |ent, cx| ent.spawn(cx)));
}

/// `Spawn` of the player the caller holds (its respawn, `respawn` 0xace58).
pub fn player_spawn(p: &mut Player, cx: &mut Cx) {
    inner(id::SPAWN, p, cx, (), |p, cx, _| p.spawn(cx));
}

/// `Think` (`DispatchThink`).
pub fn think(e: *mut edict_t) {
    outer(id::THINK, e, &mut (), |_| world::dispatch(e, |ent, cx| ent.think(cx)));
}

/// `Touch(pOther)` (`DispatchTouch` and the game's own touches).
pub fn touch(e: *mut edict_t, other: *mut edict_t) {
    outer(id::TOUCH, e, &mut (Ent(other),), |(o,)| world::dispatch(e, |ent, cx| class_touch(ent, cx, o.0)));
    // Port addition (plugin API version 6): a bot's touches, for bot backends.
    crate::plugins::entity_touch(e, other);
}

/// The class's `Touch`, unless the gameplay plugin runs its own (`ServerRules::entity_touch`).
fn class_touch(ent: &mut dyn Entity, cx: &mut Cx, other: *mut edict_t) {
    if !crate::gameplay::active().server.entity_touch(ent, cx, other) {
        ent.touch(cx, other);
    }
}

/// The class's `KeyValue`, unless the gameplay plugin parses or refuses the key first
/// (`ServerRules::entity_key_value`).
fn class_key_value(ent: &mut dyn Entity, cx: &mut Cx, key: &CStr, value: &CStr) -> bool {
    crate::gameplay::active().server.entity_key_value(ent, cx, key, value).unwrap_or_else(|| ent.key_value(cx, key, value))
}

/// `Use(pActivator, pCaller, useType, value)` (`DispatchUse` and every target fired).
pub fn use_(e: *mut edict_t, activator: *mut edict_t, caller: *mut edict_t, kind: UseType, value: f32) {
    let mut args = (Ent(activator), Ent(caller), kind, value);
    outer(id::USE, e, &mut args, |(a, c, k, v)| world::dispatch(e, |ent, cx| ent.use_(cx, a.0, c.0, *k, *v)));
}

/// `Blocked(pOther)` (`DispatchBlocked`).
pub fn blocked(e: *mut edict_t, other: *mut edict_t) {
    outer(id::BLOCKED, e, &mut (Ent(other),), |(o,)| world::dispatch(e, |ent, cx| ent.blocked(cx, o.0)));
}

/// `Activate` (`ServerActivate`).
pub fn activate(e: *mut edict_t) {
    outer(id::ACTIVATE, e, &mut (), |_| world::dispatch(e, |ent, cx| ent.activate(cx)));
}

/// `OverrideReset` (a global entity restored over this one).
pub fn override_reset(e: *mut edict_t) {
    outer(id::OVERRIDERESET, e, &mut (), |_| world::dispatch(e, |ent, cx| ent.override_reset(cx)));
}

/// `pEntity->KeyValue(pkvd)`, the class part of `DispatchKeyValue`: whether the class handled
/// the key (`fHandled`). A hook that supersedes it decides `fHandled`.
pub fn key_value(e: *mut edict_t, class: Option<&CStr>, key: &CStr, value: &CStr) -> bool {
    let run = |kvd: &mut Kvd| {
        let handled = world::dispatch(e, |ent, cx| class_key_value(ent, cx, key, value))?;
        kvd.0.fHandled = handled as c_int;
        Some(handled)
    };
    if !HOOKS.0.armed(id::KEYVALUE) {
        return world::dispatch(e, |ent, cx| class_key_value(ent, cx, key, value)) == Some(true);
    }
    let kvd = KeyValueData {
        szClassName: class.map_or(std::ptr::null_mut(), |c| c.as_ptr().cast_mut()),
        szKeyName: key.as_ptr().cast_mut(),
        szValue: value.as_ptr().cast_mut(),
        fHandled: 0,
    };
    let mut args = (Kvd(kvd),);
    let ran = std::cell::Cell::new(None);
    outer_hooked(id::KEYVALUE, e, &mut args, |(k,)| {
        ran.set(run(k));
        Some(())
    });
    ran.get().unwrap_or(args.0.0.fHandled != 0)
}

/// `TraceResult` for a hit: what the port's `DamageInfo` keeps of TS's trace.
fn trace_of(victim: *mut edict_t, info: &DamageInfo) -> TraceResult {
    TraceResult {
        fAllSolid: 0,
        fStartSolid: 0,
        fInOpen: 0,
        fInWater: 0,
        flFraction: 0.0,
        vecEndPos: info.pos,
        flPlaneDist: 0.0,
        vecPlaneNormal: [0.0; 3],
        pHit: victim,
        iHitgroup: info.hitgroup,
    }
}

type TraceArgs = (Ent, f32, [f32; 3], TraceResult, i32);

fn trace_args(victim: *mut edict_t, info: &DamageInfo) -> TraceArgs {
    (Ent(info.attacker), info.damage, info.dir, trace_of(victim, info), info.bits)
}

fn traced_info(info: &DamageInfo, (attacker, damage, dir, tr, bits): &TraceArgs) -> DamageInfo {
    DamageInfo { attacker: attacker.0, damage: *damage, dir: *dir, pos: tr.vecEndPos, hitgroup: tr.iHitgroup, bits: *bits, ..*info }
}

/// `victim->TraceAttack(pevAttacker, flDamage, vecDir, ptr, bitsDamageType)`. The hook's changes
/// to the attacker, damage, direction, bits and the trace's `iHitgroup` / `vecEndPos` are used.
pub fn trace_attack(victim: *mut edict_t, info: &DamageInfo) {
    let mut args = trace_args(victim, info);
    outer(id::TRACEATTACK, victim, &mut args, |a| {
        let info = traced_info(info, a);
        world::dispatch(victim, |ent, cx| ent.trace_attack(cx, &info))
    });
}

type DamageArgs = (Ent, Ent, f32, i32);

fn damage_info(info: &DamageInfo, (inflictor, attacker, damage, bits): &DamageArgs) -> DamageInfo {
    DamageInfo { inflictor: inflictor.0, attacker: attacker.0, damage: *damage, bits: *bits, ..*info }
}

/// `victim->TakeDamage(pevInflictor, pevAttacker, flDamage, bitsDamageType)`: whether it took
/// the damage, `None` if the victim can't be reached.
pub fn take_damage(victim: *mut edict_t, info: &DamageInfo) -> Option<bool> {
    let mut args = (Ent(info.inflictor), Ent(info.attacker), info.damage, info.bits);
    outer(id::TAKEDAMAGE, victim, &mut args, |a| {
        let info = damage_info(info, a);
        world::dispatch(victim, |ent, cx| ent.take_damage(cx, &info))
    })
}

/// `pEntity->TakeHealth(flHealth, DMG_GENERIC)`: `run` is the class's. The bits are always
/// `DMG_GENERIC` in TS's callers and the port's functions don't take them.
pub fn take_health(e: *mut edict_t, amount: f32, run: impl FnOnce(*mut edict_t, f32) -> bool) -> bool {
    outer_now(id::TAKEHEALTH, e, &mut (amount, 0), |(a, _)| Some(run(e, *a))).unwrap_or(false)
}

/// `TakeHealth` of the player the caller holds (its time-based healing, `CheckTimeBasedDamage`).
pub fn player_take_health(p: &mut Player, cx: &mut Cx, amount: f32, run: impl FnOnce(&mut Player, &mut Cx, f32) -> bool) -> bool {
    inner(id::TAKEHEALTH, p, cx, (amount, 0), |p, cx, (a, _)| run(p, cx, *a))
}

/// `Killed(pevAttacker, iGib)` of the player the caller holds (`CBasePlayer::TakeDamage`).
/// `iGib` is hamsandwich's 0 normal, 1 never, 2 always, from and back to `DMG_NEVERGIB` /
/// `DMG_ALWAYSGIB` (`CBaseMonster::TakeDamage`'s choice).
pub fn killed(p: &mut Player, cx: &mut Cx, dmg: &DamageInfo, run: impl FnOnce(&mut Player, &mut Cx, &DamageInfo)) {
    inner(id::KILLED, p, cx, (Ent(dmg.attacker), gib_of(dmg.bits)), |p, cx, (a, g)| run(p, cx, &killed_info(dmg, a.0, *g)));
}

/// `iGib` of a hit's damage bits (`CBaseMonster::TakeDamage`, SDK combat.cpp).
pub fn gib_of(bits: i32) -> i32 {
    use crate::damage::bits::{ALWAYSGIB, NEVERGIB};
    if bits & ALWAYSGIB != 0 {
        2
    } else if bits & NEVERGIB != 0 {
        1
    } else {
        0
    }
}

fn killed_info(dmg: &DamageInfo, attacker: *mut edict_t, gib: i32) -> DamageInfo {
    use crate::damage::bits::{ALWAYSGIB, NEVERGIB};
    let bits = dmg.bits & !(ALWAYSGIB | NEVERGIB)
        | match gib {
            2 => ALWAYSGIB,
            1 => NEVERGIB,
            _ => 0,
        };
    DamageInfo { attacker, bits, ..*dmg }
}

/// `PlayerPreThink`.
pub fn player_pre_think(e: *mut edict_t) {
    outer(id::PLAYER_PRETHINK, e, &mut (), |_| crate::player::with_player(e, |p, cx| p.pre_think(cx)));
}

/// `PlayerPostThink`.
pub fn player_post_think(e: *mut edict_t) {
    outer(id::PLAYER_POSTTHINK, e, &mut (), |_| crate::player::with_player(e, |p, cx| p.post_think(cx)));
    // Port addition: op 5 and the client plugin offers, once the client is spawned (issue #317).
    crate::plugin_offers::client_ready(e);
}

/// `Jump` from the player's `PreThink`.
pub fn jump(p: &mut Player, cx: &mut Cx) {
    inner(id::PLAYER_JUMP, p, cx, (), |p, cx, _| crate::stunt::jump(p, cx));
}

/// `Duck` from the player's `PreThink`.
pub fn duck(p: &mut Player, cx: &mut Cx) {
    inner(id::PLAYER_DUCK, p, cx, (), |p, cx, _| crate::stunt::duck(p, cx));
}

/// `GoSlow(amt, flag)` on any entity (`rules::powerup::go_slow` is the function).
pub fn go_slow(e: *mut edict_t, amount: f32, btime: bool, run: impl FnOnce(*mut edict_t, f32, bool)) {
    outer_now(id::TS_GOSLOW, e, &mut (amount, btime as i32), |(a, b)| {
        run(e, *a, *b != 0);
        Some(())
    });
}

/// `GoSlow(amt, flag)` of the player the caller holds: his own virtual call (`SlowMotionFlow`
/// 0x96bb0, `DeactivateSlowMotionFlow` 0x96a9c, the headshot death in `Killed`), issue #253.
pub fn player_go_slow(p: &mut Player, cx: &mut Cx, amount: f32, btime: bool, run: impl FnOnce(&mut Player, &mut Cx, f32, bool)) {
    inner(id::TS_GOSLOW, p, cx, (amount, btime as i32), |p, cx, (a, b)| run(p, cx, *a, *b != 0));
}

/// `BreakableRespawn(force)` (`CTSGameRules::RespawnEntities`); `run` is the class's.
pub fn breakable_respawn(e: *mut edict_t, force: bool, run: impl FnOnce(&mut dyn Entity, &mut Cx, bool)) {
    outer(id::TS_BREAKABLERESPAWN, e, &mut (force as i32,), |(f,)| {
        world::dispatch(e, |ent, cx| run(ent, cx, *f != 0))?;
        Some(0)
    });
}

/// `Respawn()` of the HL item the caller holds (`CItem::ItemTouch`): it returns the item.
pub fn respawn_item<T: Entity + Default>(item: &mut T, cx: &mut Cx, run: impl FnOnce(&mut T, &mut Cx)) {
    inner(id::RESPAWN, item, cx, (), |item, cx, _| {
        run(item, cx);
        Ent(cx.edict)
    });
}

/// `pfnShouldCollide`: `ShouldCollide(pOther)`.
pub fn should_collide(e: *mut edict_t, other: *mut edict_t) -> bool {
    outer(id::TS_SHOULDCOLLIDE, e, &mut (Ent(other),), |(o,)| world::dispatch(e, |ent, cx| ent.should_collide(cx, o.0))).unwrap_or(true)
}

// ---- the plugin's calls -------------------------------------------------------------------------

/// `PluginApi::ham_set_hook`. A plugin's hook replaces `ts_dev_hamlog`'s.
pub fn set_hook(hook: Option<HookFn>) {
    DEV_LOG.0.set(false);
    HOOKS.0.set_hook(hook);
}

// ---- ts_dev_hamlog ------------------------------------------------------------------------------

/// `ts_dev_hamlog`'s hook is installed.
static DEV_LOG: Global<Cell<bool>> = Global(Cell::new(false));

/// The functions `ts_dev_hamlog` watches.
const DEV_LOGGED: [usize; 3] = [id::SPAWN, id::TAKEDAMAGE, id::KILLED];

/// `ts_dev_hamlog 1|0` (sv_cheats; port addition, a dev aid): installs a hook of the server's own
/// that prints every Spawn, TakeDamage and Killed before and after (`[ts ham]` lines), the way a
/// plugin's hook sees them; 0 removes it. Refused while a plugin's hook is installed.
pub extern "C" fn cmd_dev_hamlog() {
    let eng = engine();
    if !crate::rules::cvars::dev_cheats() || eng.cmd_argc() < 2 {
        eng.print("usage (sv_cheats 1): ts_dev_hamlog 1|0\n");
        return;
    }
    let on = eng.cmd_argv(1).to_bytes() != b"0";
    let h = &HOOKS.0;
    if on && !DEV_LOG.0.get() && h.hook.get().is_some() {
        eng.print("ts_dev_hamlog: a plugin's hook is installed\n");
        return;
    }
    if on || DEV_LOG.0.get() {
        h.set_hook(on.then_some(dev_log_hook as HookFn));
        for ham in DEV_LOGGED {
            h.watch(ham, on);
        }
        DEV_LOG.0.set(on);
    }
}

fn dev_log_hook(call: &mut HamCall, post: bool) -> c_int {
    let show = |a: &HamArg| match a.kind {
        kind::ENTITY => format!("#{}", a.i),
        kind::FLOAT => format!("{}", a.f),
        kind::VECTOR => format!("{:?}", a.v),
        kind::TRACE | kind::STRING | kind::ITEMINFO => "ptr".to_owned(),
        _ => format!("{}", a.i),
    };
    let n = usize::try_from(call.argc).unwrap_or(0).min(call.args.len());
    let args: Vec<String> = call.args[..n].iter().map(show).collect();
    let ret = if post { format!(" -> {}", show(&call.orig_ret)) } else { String::new() };
    let name = usize::try_from(call.ham).ok().and_then(|i| NAMES.get(i)).copied().unwrap_or("?");
    let when = if post { "post" } else { "pre" };
    engine().print(&format!("[ts ham] {when} {name} this #{} class {} ({}){ret}\n", call.this, call.class, args.join(", ")));
    action::CONTINUE
}

/// `PluginApi::ham_watch`.
pub fn watch(ham: c_int, on: bool) -> c_int {
    match table::def(ham) {
        Some(_) => {
            HOOKS.0.watch(ham as usize, on);
            OK
        }
        None => UNSUPPORTED,
    }
}

/// `PluginApi::ham_supported`.
pub fn supported(ham: c_int, class: c_int) -> c_int {
    match (table::def(ham), classes::type_of_class(class)) {
        (Some(d), Some(ty)) => d.support(ty),
        _ => UNSUPPORTED,
    }
}

/// `PluginApi::class_id`.
pub fn class_id(classname: &CStr) -> c_int {
    classes::class_id(classname).unwrap_or(INVALID)
}

/// `PluginApi::entity_class_id`.
pub fn entity_class_id(entity: c_int) -> c_int {
    game_entity(entity).and_then(|e| world::type_of(e)).map_or(NOT_PLAYER, classes::class_of_type)
}

/// What `PluginApi::ham_execute`'s pointer arguments point to, read by `ts-server-dll`.
#[derive(Default)]
pub struct Pointers<'a> {
    /// The `TRACE` argument's `TraceResult`; the function's and hooks' changes are written back.
    pub trace: Option<&'a mut TraceResult>,
    /// `Ham_Keyvalue`'s `KeyValueData`, with its strings; `handled` is its `fHandled`.
    pub kvd: Option<KeyValue<'a>>,
}

/// A plugin's `KeyValueData`.
pub struct KeyValue<'a> {
    pub class: Option<&'a CStr>,
    pub key: &'a CStr,
    pub value: &'a CStr,
    pub handled: &'a mut c_int,
}

/// What `ham_execute` needs from the game beyond [`Env`], so tests can run without an engine.
pub(crate) trait Exec: Env {
    /// The edict of game entity `index` and its object's type.
    fn entity(&self, index: c_int) -> Option<(*mut edict_t, std::any::TypeId)>;
    /// Runs `d` ([`table::Def::run`]); `None` when the object couldn't be reached.
    fn run(&self, d: &table::Def, e: *mut edict_t, call: &mut HamCall, ptrs: &mut Pointers, hooks: bool) -> Option<()>;
}

impl Exec for Game {
    fn entity(&self, index: c_int) -> Option<(*mut edict_t, std::any::TypeId)> {
        let e = game_entity(index)?;
        Some((e, world::type_of(e)?))
    }
    fn run(&self, d: &table::Def, e: *mut edict_t, call: &mut HamCall, ptrs: &mut Pointers, hooks: bool) -> Option<()> {
        (d.run)(e, call, ptrs, hooks)
    }
}

/// `PluginApi::ham_execute`: checks, then runs `call.ham` on `call.this`.
pub fn execute(call: &mut HamCall, ptrs: &mut Pointers, hooks: bool) -> c_int {
    execute_in(&Game, &HOOKS.0, call, ptrs, hooks)
}

/// [`execute`]'s statuses, in order: `NOT_PLAYER` (not a game entity), `UNSUPPORTED` (no such
/// function for its class), `INVALID` ([`check_args`]), `BUSY` (hooks nested [`MAX_DEPTH`] deep,
/// or the object out of reach), else `OK`.
pub(crate) fn execute_in(x: &impl Exec, h: &Hooks, call: &mut HamCall, ptrs: &mut Pointers, hooks: bool) -> c_int {
    let Some((e, ty)) = x.entity(call.this) else { return NOT_PLAYER };
    let Some(d) = table::def(call.ham) else { return UNSUPPORTED };
    if d.support(ty) == UNSUPPORTED {
        return UNSUPPORTED;
    }
    if let Err(code) = check_args(x, call, d.kinds(), ptrs) {
        return code;
    }
    if h.depth() >= MAX_DEPTH {
        x.error(&format!("ham_execute({}) refused: hooks nested {MAX_DEPTH} deep", NAMES[d.ham]));
        return BUSY;
    }
    match x.run(d, e, call, ptrs, hooks) {
        Some(()) => OK,
        None => BUSY,
    }
}

/// `ham_execute`'s argument check: the count and kinds of the function's signature, entities in
/// use (or -1), and the pointers a `TRACE` argument and `Ham_Keyvalue` need.
pub(crate) fn check_args(env: &impl Env, call: &HamCall, kinds: &[c_int], ptrs: &Pointers) -> Result<(), c_int> {
    if usize::try_from(call.argc).ok() != Some(kinds.len()) {
        return Err(INVALID);
    }
    for (a, &k) in call.args.iter().zip(kinds) {
        let ok = a.kind == k
            && match k {
                kind::ENTITY => a.i == -1 || env.edict_of(a.i).is_some(),
                kind::TRACE => ptrs.trace.is_some(),
                _ => true,
            };
        if !ok {
            return Err(INVALID);
        }
    }
    if call.ham == id::KEYVALUE as c_int && ptrs.kvd.is_none() {
        return Err(INVALID);
    }
    Ok(())
}

/// Runs `run` with or without the hooks (`ExecuteHamB` / `ExecuteHam`), an outer call either way.
fn exec<A: Args, R: Ret>(hooks: bool, ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if hooks { outer(ham, e, args, run) } else { run(args) }
}

/// [`exec`] for a function that doesn't need the object taken out ([`outer_now`]).
fn exec_now<A: Args, R: Ret>(hooks: bool, ham: usize, e: *mut edict_t, args: &mut A, run: impl FnOnce(&mut A) -> Option<R>) -> Option<R> {
    if hooks { outer_now(ham, e, args, run) } else { run(args) }
}
