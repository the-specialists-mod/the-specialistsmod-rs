//! GL state saved and restored around client extensions' `redraw` on FWGS's GLES renderers
//! (`gles1`, `gles2`, `gl4es`, `gles3compat`). Port addition (issue #306): GLES has no attribute
//! stack, so where desktop GL pushes every attribute group ([`super::gl::with_saved_state`]), this
//! reads the specific state an overlay is likely to change with `glGet*` and sets it back.
//!
//! What is kept (per context generation, [`Gen`]):
//! * enables: blend, depth test, face culling, scissor test, stencil test; on GLES 1 also 2D
//!   texturing and the alpha test;
//! * the blend factors (separate on GLES 2+, with the blend equations), the depth mask and
//!   function, the viewport, the scissor box, the colour mask;
//! * the active texture unit, the 2D texture bound there and on unit 0;
//! * the pixel pack and unpack alignment;
//! * GLES 2+: the program, the array and element buffers, the framebuffer, vertex attribute
//!   arrays 0..8 enabled, and on 3+ the vertex array object;
//! * GLES 1: the current colour, the vertex/colour/texture-coordinate client arrays enabled, the
//!   matrix mode and the projection, model-view and texture matrices (pushed and popped, as
//!   GLES 1 has the stacks).
//!
//! Not kept: attribute pointers, uniforms, texture parameters, anything bound to other units or
//! targets. The save and restore logic is pure over [`GlesIo`] (unit tested with a fake); [`Real`]
//! is the context's functions, looked up as [`super::gl::proc_address`] does.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

pub const GL_BLEND: u32 = 0x0be2;
pub const GL_DEPTH_TEST: u32 = 0x0b71;
pub const GL_CULL_FACE: u32 = 0x0b44;
pub const GL_SCISSOR_TEST: u32 = 0x0c11;
pub const GL_STENCIL_TEST: u32 = 0x0b90;
pub const GL_TEXTURE_2D: u32 = 0x0de1;
pub const GL_ALPHA_TEST: u32 = 0x0bc0;
pub const GL_VERTEX_ARRAY: u32 = 0x8074;
pub const GL_COLOR_ARRAY: u32 = 0x8076;
pub const GL_TEXTURE_COORD_ARRAY: u32 = 0x8078;
const GL_BLEND_SRC: u32 = 0x0be1;
const GL_BLEND_DST: u32 = 0x0be0;
const GL_BLEND_SRC_RGB: u32 = 0x80c9;
const GL_BLEND_DST_RGB: u32 = 0x80c8;
const GL_BLEND_SRC_ALPHA: u32 = 0x80cb;
const GL_BLEND_DST_ALPHA: u32 = 0x80ca;
const GL_BLEND_EQUATION_RGB: u32 = 0x8009;
const GL_BLEND_EQUATION_ALPHA: u32 = 0x883d;
const GL_DEPTH_WRITEMASK: u32 = 0x0b72;
const GL_DEPTH_FUNC: u32 = 0x0b74;
const GL_VIEWPORT: u32 = 0x0ba2;
const GL_SCISSOR_BOX: u32 = 0x0c10;
const GL_COLOR_WRITEMASK: u32 = 0x0c23;
pub const GL_TEXTURE0: u32 = 0x84c0;
const GL_ACTIVE_TEXTURE: u32 = 0x84e0;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_CURRENT_PROGRAM: u32 = 0x8b8d;
pub const GL_ARRAY_BUFFER: u32 = 0x8892;
pub const GL_ELEMENT_ARRAY_BUFFER: u32 = 0x8893;
const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
const GL_ELEMENT_ARRAY_BUFFER_BINDING: u32 = 0x8895;
pub const GL_FRAMEBUFFER: u32 = 0x8d40;
const GL_FRAMEBUFFER_BINDING: u32 = 0x8ca6;
const GL_VERTEX_ARRAY_BINDING: u32 = 0x85b5;
pub const GL_UNPACK_ALIGNMENT: u32 = 0x0cf5;
pub const GL_PACK_ALIGNMENT: u32 = 0x0d05;
const GL_CURRENT_COLOR: u32 = 0x0b00;
const GL_MATRIX_MODE: u32 = 0x0ba0;
pub const GL_MODELVIEW: u32 = 0x1700;
pub const GL_PROJECTION: u32 = 0x1701;
pub const GL_TEXTURE: u32 = 0x1702;
const GL_VERSION: u32 = 0x1f02;

/// Vertex attribute arrays whose enable is kept (GLES 2 promises at least 8).
pub const ATTRIBS: u32 = 8;

/// The context's generation, from its `GL_VERSION` string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gen {
    /// GLES 1.x (`OpenGL ES-CM 1.1`): fixed function, matrix stacks, no programs.
    Es1,
    /// GLES 2 or later, or a desktop core context (`gles3compat` on desktop GL): programs and
    /// buffers; `vao` from GLES 3 / GL 3 on.
    Es2 { vao: bool },
}

impl Gen {
    /// Reads a `GL_VERSION` string: `OpenGL ES-CM 1.1`, `OpenGL ES 3.2 ...`, or a desktop
    /// `4.6.0 ...`. An unreadable one counts as GLES 2 without vertex array objects (the
    /// queries that are valid everywhere but GLES 1).
    pub fn from_version(v: &[u8]) -> Gen {
        let s = String::from_utf8_lossy(v);
        let rest = s.strip_prefix("OpenGL ES").map(|r| r.trim_start_matches(['-', 'C', 'M', 'L']).trim_start()).unwrap_or(&s);
        let major: u32 = rest.split(|c: char| !c.is_ascii_digit()).next().and_then(|n| n.parse().ok()).unwrap_or(2);
        match major {
            0 | 1 => Gen::Es1,
            m => Gen::Es2 { vao: m >= 3 },
        }
    }
}

/// A state change [`Saved::restore`] (or [`save`]) makes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    Enable(u32, bool),
    ClientState(u32, bool),
    AttribArray(u32, bool),
    BlendFunc(u32, u32),
    BlendFuncSeparate([u32; 4]),
    BlendEquationSeparate(u32, u32),
    DepthMask(bool),
    DepthFunc(u32),
    Viewport([i32; 4]),
    Scissor([i32; 4]),
    ColorMask([bool; 4]),
    ActiveTexture(u32),
    BindTexture(u32, u32),
    UseProgram(u32),
    BindBuffer(u32, u32),
    BindFramebuffer(u32, u32),
    BindVertexArray(u32),
    PixelStore(u32, i32),
    Color([f32; 4]),
    MatrixMode(u32),
    PushMatrix,
    PopMatrix,
}

/// The GL calls the save and restore need.
pub trait GlesIo {
    fn get_integers(&self, pname: u32, out: &mut [i32]);
    fn get_floats(&self, pname: u32, out: &mut [f32]);
    fn get_booleans(&self, pname: u32, out: &mut [bool]);
    fn is_enabled(&self, cap: u32) -> bool;
    fn attrib_enabled(&self, index: u32) -> bool;
    fn call(&self, op: Op);
}

fn int(io: &dyn GlesIo, pname: u32) -> i32 {
    let mut v = [0];
    io.get_integers(pname, &mut v);
    v[0]
}

fn caps(g: Gen) -> &'static [u32] {
    match g {
        Gen::Es1 => &[GL_BLEND, GL_DEPTH_TEST, GL_CULL_FACE, GL_SCISSOR_TEST, GL_STENCIL_TEST, GL_TEXTURE_2D, GL_ALPHA_TEST],
        Gen::Es2 { .. } => &[GL_BLEND, GL_DEPTH_TEST, GL_CULL_FACE, GL_SCISSOR_TEST, GL_STENCIL_TEST],
    }
}

/// What [`save`] read.
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub generation: Gen,
    caps: Vec<(u32, bool)>,
    client: Vec<(u32, bool)>,
    attribs: Vec<(u32, bool)>,
    blend: [u32; 4],
    equation: [u32; 2],
    depth_mask: bool,
    depth_func: u32,
    viewport: [i32; 4],
    scissor: [i32; 4],
    color_mask: [bool; 4],
    active: u32,
    tex_active: u32,
    tex0: u32,
    program: u32,
    array_buffer: u32,
    element_buffer: u32,
    framebuffer: u32,
    vao: Option<u32>,
    unpack: i32,
    pack: i32,
    color: [f32; 4],
    matrix_mode: u32,
}

/// Reads the state (module docs) and, on GLES 1, pushes the three matrices.
pub fn save(io: &dyn GlesIo, generation: Gen) -> Saved {
    let es1 = generation == Gen::Es1;
    let u = |p| int(io, p) as u32;
    let blend = if es1 {
        let (s, d) = (u(GL_BLEND_SRC), u(GL_BLEND_DST));
        [s, d, s, d]
    } else {
        [u(GL_BLEND_SRC_RGB), u(GL_BLEND_DST_RGB), u(GL_BLEND_SRC_ALPHA), u(GL_BLEND_DST_ALPHA)]
    };
    let mut viewport = [0; 4];
    io.get_integers(GL_VIEWPORT, &mut viewport);
    let mut scissor = [0; 4];
    io.get_integers(GL_SCISSOR_BOX, &mut scissor);
    let mut color_mask = [true; 4];
    io.get_booleans(GL_COLOR_WRITEMASK, &mut color_mask);
    let mut depth_mask = [true];
    io.get_booleans(GL_DEPTH_WRITEMASK, &mut depth_mask);
    let active = u(GL_ACTIVE_TEXTURE);
    let tex_active = u(GL_TEXTURE_BINDING_2D);
    let tex0 = if active == GL_TEXTURE0 {
        tex_active
    } else {
        io.call(Op::ActiveTexture(GL_TEXTURE0));
        let t = u(GL_TEXTURE_BINDING_2D);
        io.call(Op::ActiveTexture(active));
        t
    };
    let mut color = [1.0; 4];
    let mut matrix_mode = GL_MODELVIEW;
    if es1 {
        io.get_floats(GL_CURRENT_COLOR, &mut color);
        matrix_mode = u(GL_MATRIX_MODE);
        for m in [GL_PROJECTION, GL_MODELVIEW, GL_TEXTURE] {
            io.call(Op::MatrixMode(m));
            io.call(Op::PushMatrix);
        }
        io.call(Op::MatrixMode(matrix_mode));
    }
    let es2 = !es1;
    Saved {
        generation,
        caps: caps(generation).iter().map(|&c| (c, io.is_enabled(c))).collect(),
        client: if es1 {
            [GL_VERTEX_ARRAY, GL_COLOR_ARRAY, GL_TEXTURE_COORD_ARRAY].iter().map(|&c| (c, io.is_enabled(c))).collect()
        } else {
            Vec::new()
        },
        attribs: if es2 { (0..ATTRIBS).map(|i| (i, io.attrib_enabled(i))).collect() } else { Vec::new() },
        blend,
        equation: if es2 { [u(GL_BLEND_EQUATION_RGB), u(GL_BLEND_EQUATION_ALPHA)] } else { [0; 2] },
        depth_mask: depth_mask[0],
        depth_func: u(GL_DEPTH_FUNC),
        viewport,
        scissor,
        color_mask,
        active,
        tex_active,
        tex0,
        program: if es2 { u(GL_CURRENT_PROGRAM) } else { 0 },
        array_buffer: u(GL_ARRAY_BUFFER_BINDING),
        element_buffer: u(GL_ELEMENT_ARRAY_BUFFER_BINDING),
        framebuffer: if es2 { u(GL_FRAMEBUFFER_BINDING) } else { 0 },
        vao: matches!(generation, Gen::Es2 { vao: true }).then(|| u(GL_VERTEX_ARRAY_BINDING)),
        unpack: int(io, GL_UNPACK_ALIGNMENT),
        pack: int(io, GL_PACK_ALIGNMENT),
        color,
        matrix_mode,
    }
}

impl Saved {
    /// Sets everything [`save`] read back, in an order that keeps per-object state right: the
    /// vertex array object before the element buffer and attribute arrays it holds, unit 0's
    /// texture before the active unit's.
    pub fn restore(&self, io: &dyn GlesIo) {
        let es1 = self.generation == Gen::Es1;
        for &(c, on) in &self.caps {
            io.call(Op::Enable(c, on));
        }
        if es1 {
            io.call(Op::BlendFunc(self.blend[0], self.blend[1]));
        } else {
            io.call(Op::BlendFuncSeparate(self.blend));
            io.call(Op::BlendEquationSeparate(self.equation[0], self.equation[1]));
        }
        io.call(Op::DepthMask(self.depth_mask));
        io.call(Op::DepthFunc(self.depth_func));
        io.call(Op::Viewport(self.viewport));
        io.call(Op::Scissor(self.scissor));
        io.call(Op::ColorMask(self.color_mask));
        io.call(Op::PixelStore(GL_UNPACK_ALIGNMENT, self.unpack));
        io.call(Op::PixelStore(GL_PACK_ALIGNMENT, self.pack));
        if !es1 {
            io.call(Op::BindFramebuffer(GL_FRAMEBUFFER, self.framebuffer));
            io.call(Op::UseProgram(self.program));
            if let Some(v) = self.vao {
                io.call(Op::BindVertexArray(v));
            }
        }
        io.call(Op::BindBuffer(GL_ARRAY_BUFFER, self.array_buffer));
        io.call(Op::BindBuffer(GL_ELEMENT_ARRAY_BUFFER, self.element_buffer));
        for &(i, on) in &self.attribs {
            io.call(Op::AttribArray(i, on));
        }
        for &(c, on) in &self.client {
            io.call(Op::ClientState(c, on));
        }
        io.call(Op::ActiveTexture(GL_TEXTURE0));
        io.call(Op::BindTexture(GL_TEXTURE_2D, self.tex0));
        if self.active != GL_TEXTURE0 {
            io.call(Op::ActiveTexture(self.active));
            io.call(Op::BindTexture(GL_TEXTURE_2D, self.tex_active));
        }
        if es1 {
            io.call(Op::Color(self.color));
            for m in [GL_TEXTURE, GL_MODELVIEW, GL_PROJECTION] {
                io.call(Op::MatrixMode(m));
                io.call(Op::PopMatrix);
            }
            io.call(Op::MatrixMode(self.matrix_mode));
        }
    }
}

// ---- the context's functions ---------------------------------------------------------------------

// `GL_APIENTRY`: stdcall on 32-bit Windows, C elsewhere.
type U = unsafe extern "system" fn(u32);
type UU = unsafe extern "system" fn(u32, u32);
type UI = unsafe extern "system" fn(u32, i32);
type U4 = unsafe extern "system" fn(u32, u32, u32, u32);
type I4 = unsafe extern "system" fn(i32, i32, i32, i32);
type B = unsafe extern "system" fn(u8);
type B4 = unsafe extern "system" fn(u8, u8, u8, u8);
type F4 = unsafe extern "system" fn(f32, f32, f32, f32);
type V = unsafe extern "system" fn();
type IsEnabled = unsafe extern "system" fn(u32) -> u8;
type GetIv = unsafe extern "system" fn(u32, *mut i32);
type GetFv = unsafe extern "system" fn(u32, *mut f32);
type GetBv = unsafe extern "system" fn(u32, *mut u8);
type GetAttribIv = unsafe extern "system" fn(u32, u32, *mut i32);
type GetString = unsafe extern "system" fn(u32) -> *const u8;

const GL_VERTEX_ATTRIB_ARRAY_ENABLED: u32 = 0x8622;

/// The functions [`Real`] calls; a missing one is skipped (its state isn't read or kept).
struct Fns {
    get_string: Option<GetString>,
    get_integerv: Option<GetIv>,
    get_floatv: Option<GetFv>,
    get_booleanv: Option<GetBv>,
    get_vertex_attribiv: Option<GetAttribIv>,
    is_enabled: Option<IsEnabled>,
    enable: Option<U>,
    disable: Option<U>,
    enable_client_state: Option<U>,
    disable_client_state: Option<U>,
    enable_vertex_attrib_array: Option<U>,
    disable_vertex_attrib_array: Option<U>,
    blend_func: Option<UU>,
    blend_func_separate: Option<U4>,
    blend_equation_separate: Option<UU>,
    depth_mask: Option<B>,
    depth_func: Option<U>,
    viewport: Option<I4>,
    scissor: Option<I4>,
    color_mask: Option<B4>,
    active_texture: Option<U>,
    bind_texture: Option<UU>,
    use_program: Option<U>,
    bind_buffer: Option<UU>,
    bind_framebuffer: Option<UU>,
    bind_vertex_array: Option<U>,
    pixel_storei: Option<UI>,
    color4f: Option<F4>,
    matrix_mode: Option<U>,
    push_matrix: Option<V>,
    pop_matrix: Option<V>,
}

fn fns() -> &'static Fns {
    static FNS: OnceLock<Fns> = OnceLock::new();
    FNS.get_or_init(|| {
        let p = |name: &CStr| {
            let p = super::gl::proc_address(name);
            (!p.is_null()).then_some(p)
        };
        macro_rules! f {
            ($name:literal, $t:ty) => {
                // SAFETY: a non-null GL/GLES entry point of the renderer's context with this name
                // is the function with this `GL_APIENTRY` signature (gl2.h / gl.h).
                p($name).map(|f| unsafe { std::mem::transmute::<*mut c_void, $t>(f) })
            };
        }
        Fns {
            get_string: f!(c"glGetString", GetString),
            get_integerv: f!(c"glGetIntegerv", GetIv),
            get_floatv: f!(c"glGetFloatv", GetFv),
            get_booleanv: f!(c"glGetBooleanv", GetBv),
            get_vertex_attribiv: f!(c"glGetVertexAttribiv", GetAttribIv),
            is_enabled: f!(c"glIsEnabled", IsEnabled),
            enable: f!(c"glEnable", U),
            disable: f!(c"glDisable", U),
            enable_client_state: f!(c"glEnableClientState", U),
            disable_client_state: f!(c"glDisableClientState", U),
            enable_vertex_attrib_array: f!(c"glEnableVertexAttribArray", U),
            disable_vertex_attrib_array: f!(c"glDisableVertexAttribArray", U),
            blend_func: f!(c"glBlendFunc", UU),
            blend_func_separate: f!(c"glBlendFuncSeparate", U4),
            blend_equation_separate: f!(c"glBlendEquationSeparate", UU),
            depth_mask: f!(c"glDepthMask", B),
            depth_func: f!(c"glDepthFunc", U),
            viewport: f!(c"glViewport", I4),
            scissor: f!(c"glScissor", I4),
            color_mask: f!(c"glColorMask", B4),
            active_texture: f!(c"glActiveTexture", U),
            bind_texture: f!(c"glBindTexture", UU),
            use_program: f!(c"glUseProgram", U),
            bind_buffer: f!(c"glBindBuffer", UU),
            bind_framebuffer: f!(c"glBindFramebuffer", UU),
            bind_vertex_array: f!(c"glBindVertexArray", U),
            pixel_storei: f!(c"glPixelStorei", UI),
            color4f: f!(c"glColor4f", F4),
            matrix_mode: f!(c"glMatrixMode", U),
            push_matrix: f!(c"glPushMatrix", V),
            pop_matrix: f!(c"glPopMatrix", V),
        }
    })
}

/// The renderer's context, through its functions. Use only from a render callback
/// (`HUD_Redraw`), where the context is current on the calling thread.
pub struct Real(&'static Fns);

impl Real {
    /// The context's functions, or `None` without the queries the save needs.
    pub fn get() -> Option<Real> {
        let f = fns();
        (f.get_integerv.is_some() && f.is_enabled.is_some()).then_some(Real(f))
    }

    /// The context's generation (cached: the renderer can't change without restarting).
    pub fn generation(&self) -> Gen {
        static GEN: OnceLock<Gen> = OnceLock::new();
        *GEN.get_or_init(|| {
            let Some(f) = self.0.get_string else { return Gen::from_version(b"") };
            // SAFETY: `glGetString` of the current context (see the type docs); it returns NULL or
            // a static NUL-terminated string.
            let p = unsafe { f(GL_VERSION) };
            // SAFETY: as above.
            let v = if p.is_null() { &[][..] } else { unsafe { CStr::from_ptr(p.cast()) }.to_bytes() };
            Gen::from_version(v)
        })
    }
}

impl GlesIo for Real {
    fn get_integers(&self, pname: u32, out: &mut [i32]) {
        if let Some(f) = self.0.get_integerv {
            // GL writes up to four values for the names saved here; the buffer has room for four.
            let mut buf = [0i32; 4];
            buf[..out.len().min(4)].copy_from_slice(&out[..out.len().min(4)]);
            // SAFETY: the context's function (type docs) with room for the largest answer.
            unsafe { f(pname, buf.as_mut_ptr()) };
            let n = out.len().min(4);
            out[..n].copy_from_slice(&buf[..n]);
        }
    }
    fn get_floats(&self, pname: u32, out: &mut [f32]) {
        if let Some(f) = self.0.get_floatv {
            let mut buf = [0f32; 4];
            let n = out.len().min(4);
            buf[..n].copy_from_slice(&out[..n]);
            // SAFETY: as in `get_integers`.
            unsafe { f(pname, buf.as_mut_ptr()) };
            out[..n].copy_from_slice(&buf[..n]);
        }
    }
    fn get_booleans(&self, pname: u32, out: &mut [bool]) {
        if let Some(f) = self.0.get_booleanv {
            let mut buf = [0u8; 4];
            let n = out.len().min(4);
            for (b, o) in buf.iter_mut().zip(out.iter()) {
                *b = *o as u8;
            }
            // SAFETY: as in `get_integers`.
            unsafe { f(pname, buf.as_mut_ptr()) };
            for (o, b) in out[..n].iter_mut().zip(buf) {
                *o = b != 0;
            }
        }
    }
    fn is_enabled(&self, cap: u32) -> bool {
        // SAFETY: the context's function (type docs).
        self.0.is_enabled.is_some_and(|f| unsafe { f(cap) } != 0)
    }
    fn attrib_enabled(&self, index: u32) -> bool {
        let Some(f) = self.0.get_vertex_attribiv else { return false };
        let mut v = 0;
        // SAFETY: the context's function (type docs); one int answer.
        unsafe { f(index, GL_VERTEX_ATTRIB_ARRAY_ENABLED, &mut v) };
        v != 0
    }
    fn call(&self, op: Op) {
        let f = self.0;
        // SAFETY: each is the context's function (type docs), called with its signature; a
        // missing one is skipped.
        unsafe {
            match op {
                Op::Enable(c, true) => f.enable.map(|g| g(c)),
                Op::Enable(c, false) => f.disable.map(|g| g(c)),
                Op::ClientState(c, true) => f.enable_client_state.map(|g| g(c)),
                Op::ClientState(c, false) => f.disable_client_state.map(|g| g(c)),
                Op::AttribArray(i, true) => f.enable_vertex_attrib_array.map(|g| g(i)),
                Op::AttribArray(i, false) => f.disable_vertex_attrib_array.map(|g| g(i)),
                Op::BlendFunc(s, d) => f.blend_func.map(|g| g(s, d)),
                Op::BlendFuncSeparate([a, b, c, d]) => f.blend_func_separate.map(|g| g(a, b, c, d)),
                Op::BlendEquationSeparate(a, b) => f.blend_equation_separate.map(|g| g(a, b)),
                Op::DepthMask(m) => f.depth_mask.map(|g| g(m as u8)),
                Op::DepthFunc(d) => f.depth_func.map(|g| g(d)),
                Op::Viewport([x, y, w, h]) => f.viewport.map(|g| g(x, y, w, h)),
                Op::Scissor([x, y, w, h]) => f.scissor.map(|g| g(x, y, w, h)),
                Op::ColorMask([r, g_, b, a]) => f.color_mask.map(|g| g(r as u8, g_ as u8, b as u8, a as u8)),
                Op::ActiveTexture(u) => f.active_texture.map(|g| g(u)),
                Op::BindTexture(t, id) => f.bind_texture.map(|g| g(t, id)),
                Op::UseProgram(p) => f.use_program.map(|g| g(p)),
                Op::BindBuffer(t, id) => f.bind_buffer.map(|g| g(t, id)),
                Op::BindFramebuffer(t, id) => f.bind_framebuffer.map(|g| g(t, id)),
                Op::BindVertexArray(v) => f.bind_vertex_array.map(|g| g(v)),
                Op::PixelStore(p, v) => f.pixel_storei.map(|g| g(p, v)),
                Op::Color([r, g_, b, a]) => f.color4f.map(|g| g(r, g_, b, a)),
                Op::MatrixMode(m) => f.matrix_mode.map(|g| g(m)),
                Op::PushMatrix => f.push_matrix.map(|g| g()),
                Op::PopMatrix => f.pop_matrix.map(|g| g()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A context that keeps the state the ops set, and the ops in order.
    #[derive(Default)]
    struct Fake {
        ints: RefCell<HashMap<u32, Vec<i32>>>,
        caps: RefCell<HashMap<u32, bool>>,
        attribs: RefCell<HashMap<u32, bool>>,
        /// Unit -> bound 2D texture.
        textures: RefCell<HashMap<u32, u32>>,
        ops: RefCell<Vec<Op>>,
        stack: RefCell<Vec<u32>>,
    }

    impl Fake {
        fn set(&self, p: u32, v: &[i32]) {
            self.ints.borrow_mut().insert(p, v.to_vec());
        }
        fn get(&self, p: u32) -> Vec<i32> {
            self.ints.borrow().get(&p).cloned().unwrap_or_default()
        }
        fn active(&self) -> u32 {
            self.get(GL_ACTIVE_TEXTURE).first().copied().unwrap_or(GL_TEXTURE0 as i32) as u32
        }
    }

    impl GlesIo for Fake {
        fn get_integers(&self, p: u32, out: &mut [i32]) {
            let v = if p == GL_TEXTURE_BINDING_2D {
                vec![self.textures.borrow().get(&self.active()).copied().unwrap_or(0) as i32]
            } else {
                self.get(p)
            };
            for (o, x) in out.iter_mut().zip(v) {
                *o = x;
            }
        }
        fn get_floats(&self, p: u32, out: &mut [f32]) {
            for (o, x) in out.iter_mut().zip(self.get(p)) {
                *o = x as f32;
            }
        }
        fn get_booleans(&self, p: u32, out: &mut [bool]) {
            for (o, x) in out.iter_mut().zip(self.get(p)) {
                *o = x != 0;
            }
        }
        fn is_enabled(&self, c: u32) -> bool {
            self.caps.borrow().get(&c).copied().unwrap_or(false)
        }
        fn attrib_enabled(&self, i: u32) -> bool {
            self.attribs.borrow().get(&i).copied().unwrap_or(false)
        }
        fn call(&self, op: Op) {
            self.ops.borrow_mut().push(op);
            match op {
                Op::Enable(c, on) | Op::ClientState(c, on) => drop(self.caps.borrow_mut().insert(c, on)),
                Op::AttribArray(i, on) => drop(self.attribs.borrow_mut().insert(i, on)),
                Op::BlendFuncSeparate([a, b, c, d]) => {
                    for (p, v) in [(GL_BLEND_SRC_RGB, a), (GL_BLEND_DST_RGB, b), (GL_BLEND_SRC_ALPHA, c), (GL_BLEND_DST_ALPHA, d)] {
                        self.set(p, &[v as i32]);
                    }
                }
                Op::BlendFunc(s, d) => {
                    self.set(GL_BLEND_SRC, &[s as i32]);
                    self.set(GL_BLEND_DST, &[d as i32]);
                }
                Op::Viewport(v) => self.set(GL_VIEWPORT, &v),
                Op::Scissor(v) => self.set(GL_SCISSOR_BOX, &v),
                Op::DepthMask(m) => self.set(GL_DEPTH_WRITEMASK, &[m as i32]),
                Op::ActiveTexture(u) => self.set(GL_ACTIVE_TEXTURE, &[u as i32]),
                Op::BindTexture(_, id) => drop(self.textures.borrow_mut().insert(self.active(), id)),
                Op::UseProgram(p) => self.set(GL_CURRENT_PROGRAM, &[p as i32]),
                Op::BindBuffer(GL_ARRAY_BUFFER, id) => self.set(GL_ARRAY_BUFFER_BINDING, &[id as i32]),
                Op::BindBuffer(_, id) => self.set(GL_ELEMENT_ARRAY_BUFFER_BINDING, &[id as i32]),
                Op::BindFramebuffer(_, id) => self.set(GL_FRAMEBUFFER_BINDING, &[id as i32]),
                Op::BindVertexArray(v) => self.set(GL_VERTEX_ARRAY_BINDING, &[v as i32]),
                Op::PixelStore(p, v) => self.set(p, &[v]),
                Op::MatrixMode(m) => self.set(GL_MATRIX_MODE, &[m as i32]),
                Op::PushMatrix => self.stack.borrow_mut().push(self.get(GL_MATRIX_MODE)[0] as u32),
                Op::PopMatrix => {
                    assert_eq!(self.stack.borrow_mut().pop(), Some(self.get(GL_MATRIX_MODE)[0] as u32), "popped the wrong stack")
                }
                _ => {}
            }
        }
    }

    #[test]
    fn versions() {
        assert_eq!(Gen::from_version(b"OpenGL ES-CM 1.1"), Gen::Es1);
        assert_eq!(Gen::from_version(b"OpenGL ES-CL 1.0"), Gen::Es1);
        assert_eq!(Gen::from_version(b"OpenGL ES 2.0 Mesa 24.0"), Gen::Es2 { vao: false });
        assert_eq!(Gen::from_version(b"OpenGL ES 3.2 NVIDIA 550"), Gen::Es2 { vao: true });
        assert_eq!(Gen::from_version(b"4.6.0 NVIDIA 550.54"), Gen::Es2 { vao: true });
        assert_eq!(Gen::from_version(b""), Gen::Es2 { vao: false });
    }

    /// What an overlay might change, put back as it was; the vertex array object goes back
    /// before the buffers and attribute arrays it holds.
    #[test]
    fn gles2_state_comes_back() {
        let gl = Fake::default();
        gl.set(GL_VIEWPORT, &[0, 0, 1024, 768]);
        gl.set(GL_SCISSOR_BOX, &[1, 2, 3, 4]);
        gl.set(GL_CURRENT_PROGRAM, &[7]);
        gl.set(GL_ACTIVE_TEXTURE, &[GL_TEXTURE0 as i32 + 1]);
        gl.set(GL_UNPACK_ALIGNMENT, &[4]);
        gl.set(GL_VERTEX_ARRAY_BINDING, &[3]);
        gl.set(GL_ELEMENT_ARRAY_BUFFER_BINDING, &[9]);
        gl.set(GL_DEPTH_WRITEMASK, &[1]);
        gl.textures.borrow_mut().extend([(GL_TEXTURE0, 11), (GL_TEXTURE0 + 1, 12)]);
        gl.caps.borrow_mut().insert(GL_DEPTH_TEST, true);
        gl.attribs.borrow_mut().insert(0, true);
        let before = save(&gl, Gen::Es2 { vao: true });
        assert_eq!(gl.active(), GL_TEXTURE0 + 1, "reading unit 0 put the active unit back");

        // The overlay's own state.
        for op in [
            Op::Enable(GL_BLEND, true),
            Op::Enable(GL_DEPTH_TEST, false),
            Op::Viewport([0, 0, 64, 64]),
            Op::UseProgram(99),
            Op::ActiveTexture(GL_TEXTURE0),
            Op::BindTexture(GL_TEXTURE_2D, 50),
            Op::PixelStore(GL_UNPACK_ALIGNMENT, 1),
            Op::BindVertexArray(5),
            Op::BindBuffer(GL_ELEMENT_ARRAY_BUFFER, 6),
            Op::AttribArray(0, false),
            Op::AttribArray(3, true),
            Op::DepthMask(false),
        ] {
            gl.call(op);
        }
        assert_ne!(save(&gl, Gen::Es2 { vao: true }), before);
        gl.ops.borrow_mut().clear();
        before.restore(&gl);
        assert_eq!(save(&gl, Gen::Es2 { vao: true }), before);
        assert_eq!(gl.textures.borrow()[&GL_TEXTURE0], 11);
        assert_eq!(gl.textures.borrow()[&(GL_TEXTURE0 + 1)], 12);
        let ops = gl.ops.borrow();
        let at = |o: Op| ops.iter().position(|x| *x == o).expect("restored");
        assert!(at(Op::BindVertexArray(3)) < at(Op::BindBuffer(GL_ELEMENT_ARRAY_BUFFER, 9)));
        assert!(at(Op::BindVertexArray(3)) < at(Op::AttribArray(0, true)));
        assert!(!ops.iter().any(|o| matches!(o, Op::PushMatrix | Op::PopMatrix | Op::Color(_) | Op::ClientState(..))), "no GLES 1 calls");
    }

    #[test]
    fn gles1_pushes_the_matrices_and_keeps_fixed_function_state() {
        let gl = Fake::default();
        gl.set(GL_MATRIX_MODE, &[GL_MODELVIEW as i32]);
        gl.set(GL_BLEND_SRC, &[0x302]);
        gl.set(GL_BLEND_DST, &[0x303]);
        gl.caps.borrow_mut().insert(GL_TEXTURE_2D, true);
        let before = save(&gl, Gen::Es1);
        assert_eq!(gl.stack.borrow().len(), 3, "three matrices pushed");
        gl.call(Op::Enable(GL_TEXTURE_2D, false));
        gl.call(Op::ClientState(GL_COLOR_ARRAY, true));
        gl.call(Op::BlendFunc(1, 0));
        gl.call(Op::MatrixMode(GL_PROJECTION));
        gl.ops.borrow_mut().clear();
        before.restore(&gl);
        assert!(gl.stack.borrow().is_empty(), "each push popped, from its own stack");
        assert_eq!(gl.get(GL_MATRIX_MODE), vec![GL_MODELVIEW as i32]);
        assert!(gl.is_enabled(GL_TEXTURE_2D) && !gl.is_enabled(GL_COLOR_ARRAY));
        assert_eq!((gl.get(GL_BLEND_SRC), gl.get(GL_BLEND_DST)), (vec![0x302], vec![0x303]));
        let ops = gl.ops.borrow();
        assert!(
            !ops.iter().any(|o| matches!(o, Op::UseProgram(_) | Op::BindFramebuffer(..) | Op::AttribArray(..) | Op::BlendFuncSeparate(_))),
            "no GLES 2 calls"
        );
    }
}
