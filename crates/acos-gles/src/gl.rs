//! The GLES2 object model + state machine (ADR-0023).
//!
//! Everything here is safe Rust over plain data: buffers, textures, shader/
//! program objects, framebuffer attachments and the fixed-function state block.
//! The `unsafe extern "C"` entry points in [`crate::ffi`] only unpack C
//! arguments into the typed calls in this module.
//!
//! Entry points outside the implemented subset do not exist as methods at all;
//! the FFI layer routes them to [`crate::error::unsupported`] and sets
//! `GL_INVALID_OPERATION`, so an unknown call can never be mistaken for a
//! success (ADR-0023 "no silent no-ops").

use std::collections::HashMap;

use crate::error::{consts as c, diagnostic, unsupported};
use crate::glsl::{self, Stage, TextureLookup, Ty, Unit, Value, VertexOut};
use crate::raster::{
    project, Blend, BlendFactor, Cull, DepthFunc, Framebuffer, RasterState,
};

/// One shader object's compile result.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct ShaderObj {
    pub stage: Stage,
    pub source: String,
    /// `true` when the GLSL was accepted by the interpreter.
    pub accepted: bool,
    /// Compile log (`glGetShaderInfoLog`).
    pub log: String,
    /// Parsed unit, present iff `accepted`.
    pub unit: Option<Unit>,
    pub delete_status: bool,
}

/// A linked program.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Program {
    pub vs: u32,
    pub fs: u32,
    pub linked: bool,
    pub log: String,
    pub vs_unit: Unit,
    pub fs_unit: Unit,
    /// Uniform names in location order (`glGetUniformLocation` slot).
    pub uniform_order: Vec<String>,
    /// Attribute names in location order.
    pub attrib_order: Vec<String>,
    /// Explicit `glBindAttribLocation` bindings.
    pub attrib_bindings: HashMap<String, u32>,
    pub delete_status: bool,
}

impl Program {
    /// Attribute location for `name`, if the program uses it.
    #[must_use]
    pub fn attrib_location(&self, name: &str) -> Option<u32> {
        if let Some(bound) = self.attrib_bindings.get(name) {
            return Some(*bound);
        }
        self.attrib_order.iter().position(|n| n == name).map(|i| i as u32)
    }

    /// Uniform slot for `name`.
    #[must_use]
    pub fn uniform_slot(&self, name: &str) -> Option<usize> {
        self.uniform_order.iter().position(|n| n == name)
    }
}

/// `glVertexAttribPointer` state for one attribute slot.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct AttribArray {
    pub buffer: u32,
    pub size: i32,
    pub ty: u32,
    pub normalized: bool,
    pub stride: i32,
    pub offset: i32,
}

impl Default for AttribArray {
    fn default() -> AttribArray {
        AttribArray {
            buffer: 0,
            size: 4,
            ty: c::FLOAT,
            normalized: false,
            stride: 0,
            offset: 0,
        }
    }
}

/// A texture object (always RGBA8888 internally).
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Texture {
    pub width: u32,
    pub height: u32,
    /// Straight RGBA8888.
    pub rgba: Vec<u8>,
    pub min_filter: u32,
    pub mag_filter: u32,
    pub wrap_s: u32,
    pub wrap_t: u32,
}

impl Texture {
    fn empty() -> Texture {
        Texture {
            width: 0,
            height: 0,
            rgba: Vec::new(),
            min_filter: c::NEAREST_MIPMAP_LINEAR,
            mag_filter: c::LINEAR,
            wrap_s: c::REPEAT,
            wrap_t: c::REPEAT,
        }
    }

    fn allocated(&self) -> bool {
        self.width > 0 && self.height > 0 && self.rgba.len() >= self.width as usize * self.height as usize * 4
    }
}

/// A renderbuffer object (`glRenderbufferStorage`).
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Renderbuffer {
    pub width: u32,
    pub height: u32,
}

/// The per-context GL state.
#[derive(Debug)]
#[allow(missing_docs)]
pub struct Gl {
    pub buffers: HashMap<u32, Vec<u8>>,
    pub textures: HashMap<u32, Texture>,
    pub shaders: HashMap<u32, ShaderObj>,
    pub programs: HashMap<u32, Program>,
    pub framebuffers: HashMap<u32, Box<Framebuffer>>,
    /// `fbo -> (colour texture, depth texture)`
    pub fbo_color: HashMap<u32, u32>,
    pub fbo_depth: HashMap<u32, u32>,
    pub renderbuffers: HashMap<u32, Renderbuffer>,
    pub fbo_depth_rb: HashMap<u32, u32>,
    next_id: u32,

    // bindings
    pub array_buffer: u32,
    pub element_buffer: u32,
    pub active_texture: usize,
    pub bound_textures: [u32; c::TEXTURE_UNITS],
    pub current_program: u32,
    pub framebuffer: u32,
    pub renderbuffer: u32,

    // vertex attributes
    pub attrib_arrays: HashMap<u32, AttribArray>,
    pub attrib_enabled: [bool; c::MAX_ATTRIBS],
    pub attrib_generic: [[f32; 4]; c::MAX_ATTRIBS],

    // uniforms per program
    pub uniforms: HashMap<u32, HashMap<String, Value>>,

    // fixed-function state
    pub clear_color: [f32; 4],
    pub clear_depth: f32,
    pub depth_test: bool,
    pub depth_func: u32,
    pub depth_mask: bool,
    pub blend: bool,
    pub blend_rgb: (u32, u32),
    pub blend_alpha: (u32, u32),
    pub cull: bool,
    pub cull_face: u32,
    pub front_ccw: bool,
    pub scissor_test: bool,
    pub scissor: (i32, i32, u32, u32),
    pub viewport: (i32, i32, u32, u32),
    pub color_mask: [bool; 4],
    pub unpack_alignment: i32,
    pub line_width: f32,
    pub error: u32,
}

impl Default for Gl {
    fn default() -> Gl {
        Gl::new()
    }
}

impl Gl {
    /// Fresh context state (GLES2 initial values).
    #[must_use]
    pub fn new() -> Gl {
        Gl {
            buffers: HashMap::new(),
            textures: HashMap::new(),
            shaders: HashMap::new(),
            programs: HashMap::new(),
            framebuffers: HashMap::new(),
            fbo_color: HashMap::new(),
            fbo_depth: HashMap::new(),
            renderbuffers: HashMap::new(),
            fbo_depth_rb: HashMap::new(),
            next_id: 1,
            array_buffer: 0,
            element_buffer: 0,
            active_texture: 0,
            bound_textures: [0; c::TEXTURE_UNITS],
            current_program: 0,
            framebuffer: 0,
            renderbuffer: 0,
            attrib_arrays: HashMap::new(),
            attrib_enabled: [false; c::MAX_ATTRIBS],
            attrib_generic: [[0.0, 0.0, 0.0, 1.0]; c::MAX_ATTRIBS],
            uniforms: HashMap::new(),
            clear_color: [0.0; 4],
            clear_depth: 1.0,
            depth_test: false,
            depth_func: c::LESS,
            depth_mask: true,
            blend: false,
            blend_rgb: (c::ONE, c::ZERO),
            blend_alpha: (c::ONE, c::ZERO),
            cull: false,
            cull_face: c::BACK,
            front_ccw: true,
            scissor_test: false,
            scissor: (0, 0, 0, 0),
            viewport: (0, 0, 0, 0),
            color_mask: [true; 4],
            unpack_alignment: 4,
            line_width: 1.0,
            error: c::NO_ERROR,
        }
    }

    fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Record a GL error (first error wins, as GLES2 specifies).
    pub fn set_error(&mut self, code: u32) {
        if self.error == c::NO_ERROR {
            self.error = code;
        }
    }

    /// `glGetError`.
    pub fn take_error(&mut self) -> u32 {
        std::mem::replace(&mut self.error, c::NO_ERROR)
    }

    // -- buffers ---------------------------------------------------------

    /// `glGenBuffers`.
    pub fn gen_buffers(&mut self, n: i32) -> Vec<u32> {
        (0..n.max(0)).map(|_| {
            let id = self.id();
            self.buffers.insert(id, Vec::new());
            id
        }).collect()
    }

    /// `glDeleteBuffers`.
    pub fn delete_buffers(&mut self, ids: &[u32]) {
        for id in ids {
            self.buffers.remove(id);
        }
    }

    /// `glBindBuffer`.
    pub fn bind_buffer(&mut self, target: u32, id: u32) {
        if id != 0 && !self.buffers.contains_key(&id) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        match target {
            c::ARRAY_BUFFER => self.array_buffer = id,
            c::ELEMENT_ARRAY_BUFFER => self.element_buffer = id,
            other => {
                unsupported("glBindBuffer", format!("unknown target {other:#x}"));
                self.set_error(c::INVALID_ENUM);
            }
        }
    }

    /// `glBufferData`.
    pub fn buffer_data(&mut self, target: u32, data: &[u8], usage: u32) {
        if !matches!(usage, c::STATIC_DRAW | c::DYNAMIC_DRAW | c::STREAM_DRAW) {
            unsupported("glBufferData", format!("usage {usage:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        let id = match target {
            c::ARRAY_BUFFER => self.array_buffer,
            c::ELEMENT_ARRAY_BUFFER => self.element_buffer,
            other => {
                unsupported("glBufferData", format!("unknown target {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                return;
            }
        };
        match self.buffers.get_mut(&id) {
            Some(buf) => {
                buf.clear();
                buf.extend_from_slice(data);
            }
            None => self.set_error(c::INVALID_OPERATION),
        }
    }

    /// `glBufferSubData`.
    pub fn buffer_sub_data(&mut self, target: u32, offset: i32, data: &[u8]) {
        let id = match target {
            c::ARRAY_BUFFER => self.array_buffer,
            c::ELEMENT_ARRAY_BUFFER => self.element_buffer,
            other => {
                unsupported("glBufferSubData", format!("unknown target {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                return;
            }
        };
        if offset < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        match self.buffers.get_mut(&id) {
            Some(buf) => {
                let off = offset as usize;
                if off + data.len() > buf.len() {
                    self.set_error(c::INVALID_VALUE);
                    return;
                }
                buf[off..off + data.len()].copy_from_slice(data);
            }
            None => self.set_error(c::INVALID_OPERATION),
        }
    }

    // -- shaders / programs ---------------------------------------------

    /// `glCreateShader`.
    pub fn create_shader(&mut self, ty: u32) -> u32 {
        let stage = match ty {
            c::VERTEX_SHADER => Stage::Vertex,
            c::FRAGMENT_SHADER => Stage::Fragment,
            other => {
                unsupported("glCreateShader", format!("shader type {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                return 0;
            }
        };
        let id = self.id();
        self.shaders.insert(
            id,
            ShaderObj {
                stage,
                source: String::new(),
                accepted: false,
                log: String::new(),
                unit: None,
                delete_status: false,
            },
        );
        id
    }

    /// `glDeleteShader`.
    pub fn delete_shader(&mut self, id: u32) {
        if let Some(s) = self.shaders.get_mut(&id) {
            s.delete_status = true;
            if !self.programs.values().any(|p| p.vs == id || p.fs == id) {
                self.shaders.remove(&id);
            }
        }
    }

    /// `glShaderSource`.
    pub fn shader_source(&mut self, id: u32, source: String) {
        match self.shaders.get_mut(&id) {
            Some(s) => s.source = source,
            None => self.set_error(c::INVALID_VALUE),
        }
    }

    /// `glCompileShader` — the structured accept/reject point for GLSL.
    pub fn compile_shader(&mut self, id: u32) {
        let Some(shader) = self.shaders.get(&id) else {
            self.set_error(c::INVALID_VALUE);
            return;
        };
        let stage = shader.stage;
        let source = shader.source.clone();
        if source.trim().is_empty() {
            if let Some(s) = self.shaders.get_mut(&id) {
                s.accepted = false;
                s.log = "shader has no source".into();
                s.unit = None;
            }
            unsupported("glCompileShader", "empty shader source");
            return;
        }
        match glsl::parse(&source, stage) {
            Ok(unit) => {
                if let Some(s) = self.shaders.get_mut(&id) {
                    s.accepted = true;
                    s.log = String::new();
                    s.unit = Some(unit);
                }
            }
            Err(e) => {
                let msg = format!("{} shader rejected: {e}", stage.name());
                if let Some(s) = self.shaders.get_mut(&id) {
                    s.accepted = false;
                    s.log = msg.clone();
                    s.unit = None;
                }
                unsupported("glCompileShader", msg);
            }
        }
    }

    /// `glGetShaderiv`.
    pub fn get_shader_iv(&mut self, id: u32, pname: u32) -> i32 {
        let Some(s) = self.shaders.get(&id) else {
            self.set_error(c::INVALID_VALUE);
            return 0;
        };
        match pname {
            c::COMPILE_STATUS => i32::from(s.accepted),
            c::DELETE_STATUS => i32::from(s.delete_status),
            c::INFO_LOG_LENGTH => (s.log.len() + 1) as i32,
            other => {
                unsupported("glGetShaderiv", format!("pname {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                0
            }
        }
    }

    /// `glGetShaderInfoLog`.
    #[must_use]
    pub fn shader_info_log(&self, id: u32) -> String {
        self.shaders.get(&id).map(|s| s.log.clone()).unwrap_or_default()
    }

    /// `glCreateProgram`.
    pub fn create_program(&mut self) -> u32 {
        let id = self.id();
        self.programs.insert(
            id,
            Program {
                vs: 0,
                fs: 0,
                linked: false,
                log: String::new(),
                vs_unit: Unit::default(),
                fs_unit: Unit::default(),
                uniform_order: Vec::new(),
                attrib_order: Vec::new(),
                attrib_bindings: HashMap::new(),
                delete_status: false,
            },
        );
        id
    }

    /// `glDeleteProgram`.
    pub fn delete_program(&mut self, id: u32) {
        if let Some(p) = self.programs.get_mut(&id) {
            p.delete_status = true;
        }
        if self.current_program == id {
            self.current_program = 0;
        }
        self.programs.remove(&id);
        self.uniforms.remove(&id);
    }

    /// `glAttachShader`.
    pub fn attach_shader(&mut self, program: u32, shader: u32) {
        let kind = self.shaders.get(&shader).map(|s| s.stage);
        let Some(kind) = kind else {
            self.set_error(c::INVALID_VALUE);
            return;
        };
        let Some(p) = self.programs.get_mut(&program) else {
            self.set_error(c::INVALID_VALUE);
            return;
        };
        match kind {
            Stage::Vertex => p.vs = shader,
            Stage::Fragment => p.fs = shader,
        }
    }

    /// `glBindAttribLocation`.
    pub fn bind_attrib_location(&mut self, program: u32, index: u32, name: &str) {
        if index as usize >= c::MAX_ATTRIBS {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        match self.programs.get_mut(&program) {
            Some(p) => {
                p.attrib_bindings.insert(name.to_string(), index);
            }
            None => self.set_error(c::INVALID_VALUE),
        }
    }

    /// `glLinkProgram` — resolves interfaces, uniform slots and attribute slots.
    pub fn link_program(&mut self, program: u32) {
        let (vs_id, fs_id) = match self.programs.get(&program) {
            Some(p) => (p.vs, p.fs),
            None => {
                self.set_error(c::INVALID_VALUE);
                return;
            }
        };
        if vs_id == 0 || fs_id == 0 {
            if let Some(p) = self.programs.get_mut(&program) {
                p.linked = false;
                p.log = "program needs one vertex and one fragment shader".into();
            }
            unsupported("glLinkProgram", "program is missing a shader stage");
            return;
        }
        let vs_unit = self.shaders.get(&vs_id).and_then(|s| s.unit.clone());
        let fs_unit = self.shaders.get(&fs_id).and_then(|s| s.unit.clone());
        let (Some(vs_unit), Some(fs_unit)) = (vs_unit, fs_unit) else {
            if let Some(p) = self.programs.get_mut(&program) {
                p.linked = false;
                p.log = "attached shader did not compile".into();
            }
            unsupported("glLinkProgram", "attached shader did not compile");
            return;
        };
        let vs_iface = vs_unit.interface();
        let fs_iface = fs_unit.interface();
        let mut uniform_order = vs_iface.uniforms.clone();
        for name in &fs_iface.uniforms {
            if !uniform_order.contains(name) {
                uniform_order.push(name.clone());
            }
        }
        let mut attrib_order = vs_iface.attributes.clone();
        // Reserve slots for explicitly bound attributes.
        let bindings = self
            .programs
            .get(&program)
            .map(|p| p.attrib_bindings.clone())
            .unwrap_or_default();
        for name in &attrib_order {
            if let Some(slot) = bindings.get(name) {
                if *slot as usize >= c::MAX_ATTRIBS {
                    continue;
                }
            }
        }
        attrib_order.retain(|n| !bindings.contains_key(n));
        let mut used: Vec<u32> = bindings
            .values()
            .copied()
            .filter(|s| (*s as usize) < c::MAX_ATTRIBS)
            .collect();
        used.sort_unstable();
        let mut next = 0u32;
        for name in attrib_order.iter() {
            while used.contains(&next) {
                next += 1;
            }
            used.push(next);
            next += 1;
            let _ = name;
        }
        let over = self.uniforms.entry(program).or_default();
        over.clear();
        let mut p = self.programs.remove(&program).expect("program exists");
        p.vs_unit = vs_unit;
        p.fs_unit = fs_unit;
        p.uniform_order = uniform_order.clone();
        p.attrib_order = attrib_order;
        p.linked = true;
        p.log = String::new();
        self.programs.insert(program, p);
        // Sampler uniforms default to unit 0; everything else to zero.
        if let Some(u) = self.uniforms.get_mut(&program) {
            for name in uniform_order {
                let is_sampler = self
                    .programs
                    .get(&program)
                    .map(|p| {
                        p.vs_unit
                            .globals
                            .iter()
                            .chain(p.fs_unit.globals.iter())
                            .any(|d| d.name == name && d.ty == Ty::Sampler2D)
                    })
                    .unwrap_or(false);
                u.insert(
                    name,
                    if is_sampler {
                        Value::S(0)
                    } else {
                        Value::vec4(0.0, 0.0, 0.0, 0.0)
                    },
                );
            }
        }
    }

    /// `glGetProgramiv`.
    pub fn get_program_iv(&mut self, program: u32, pname: u32) -> i32 {
        let Some(p) = self.programs.get(&program) else {
            self.set_error(c::INVALID_VALUE);
            return 0;
        };
        match pname {
            c::LINK_STATUS => i32::from(p.linked),
            c::VALIDATE_STATUS => i32::from(p.linked),
            c::DELETE_STATUS => i32::from(p.delete_status),
            c::INFO_LOG_LENGTH => (p.log.len() + 1) as i32,
            c::ACTIVE_UNIFORMS => p.uniform_order.len() as i32,
            c::ACTIVE_ATTRIBUTES => p.attrib_order.len() as i32,
            other => {
                unsupported("glGetProgramiv", format!("pname {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                0
            }
        }
    }

    /// `glGetProgramInfoLog`.
    #[must_use]
    pub fn program_info_log(&self, program: u32) -> String {
        self.programs.get(&program).map(|p| p.log.clone()).unwrap_or_default()
    }

    /// `glUseProgram`.
    pub fn use_program(&mut self, program: u32) {
        if program != 0 && !self.programs.contains_key(&program) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        self.current_program = program;
    }

    /// `glGetUniformLocation`, encoded as `(program << 8) | slot`.
    #[must_use]
    pub fn get_uniform_location(&self, program: u32, name: &str) -> i32 {
        let Some(p) = self.programs.get(&program) else {
            return -1;
        };
        if name.contains('[') {
            // Uniform arrays are outside the interpreted subset: the name is
            // not resolvable, which GL reports as -1.
            return -1;
        }
        match p.uniform_slot(name) {
            Some(slot) if slot < 256 => ((program as i32) << 8) | slot as i32,
            _ => -1,
        }
    }

    /// `glGetAttribLocation`.
    #[must_use]
    pub fn get_attrib_location(&self, program: u32, name: &str) -> i32 {
        self.programs
            .get(&program)
            .and_then(|p| p.attrib_location(name))
            .map_or(-1, |l| l as i32)
    }

    /// Resolve a uniform location into `(program, name)`.
    fn resolve_uniform(&self, location: i32) -> Option<(u32, String)> {
        if location < 0 {
            return None;
        }
        let program = (location >> 8) as u32;
        let slot = (location & 0xff) as usize;
        self.programs
            .get(&program)
            .and_then(|p| p.uniform_order.get(slot).cloned())
            .map(|n| (program, n))
    }

    /// `glUniform*` for a single value.
    pub fn uniform_value(&mut self, location: i32, value: Value) {
        let Some((program, name)) = self.resolve_uniform(location) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        self.uniforms
            .entry(program)
            .or_default()
            .insert(name, value);
    }

    /// `glUniformMatrix4fv` (and the 2x2/3x3 forms).
    pub fn uniform_matrix(&mut self, location: i32, n: usize, transpose: bool, data: &[f32]) {
        let Some((program, name)) = self.resolve_uniform(location) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        if data.len() < n * n {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        let mut m = [0.0f32; 16];
        if transpose {
            for r in 0..n {
                for col in 0..n {
                    m[col * n + r] = data[r * n + col];
                }
            }
        } else {
            // GL hands matrices in column-major order.
            m[..n * n].copy_from_slice(&data[..n * n]);
        }
        self.uniforms
            .entry(program)
            .or_default()
            .insert(name, Value::M(m, n));
    }

    /// `glVertexAttribPointer`.
    pub fn vertex_attrib_pointer(
        &mut self,
        index: u32,
        size: i32,
        ty: u32,
        normalized: bool,
        stride: i32,
        offset: i32,
    ) {
        if index as usize >= c::MAX_ATTRIBS || !(1..=4).contains(&size) || stride < 0 || offset < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        if !matches!(
            ty,
            c::BYTE | c::UNSIGNED_BYTE | c::SHORT | c::UNSIGNED_SHORT | c::FLOAT | c::FIXED
        ) {
            unsupported("glVertexAttribPointer", format!("type {ty:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.attrib_arrays.insert(
            index,
            AttribArray {
                buffer: self.array_buffer,
                size,
                ty,
                normalized,
                stride,
                offset,
            },
        );
    }

    /// `glEnableVertexAttribArray` / `glDisableVertexAttribArray`.
    pub fn set_attrib_array_enabled(&mut self, index: u32, enabled: bool) {
        if index as usize >= c::MAX_ATTRIBS {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        self.attrib_enabled[index as usize] = enabled;
    }

    /// `glVertexAttrib[1-4]f` constant attribute value.
    pub fn vertex_attrib4f(&mut self, index: u32, v: [f32; 4]) {
        if index as usize >= c::MAX_ATTRIBS {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        self.attrib_generic[index as usize] = v;
    }

    // -- textures --------------------------------------------------------

    /// `glGenTextures`.
    pub fn gen_textures(&mut self, n: i32) -> Vec<u32> {
        (0..n.max(0))
            .map(|_| {
                let id = self.id();
                self.textures.insert(id, Texture::empty());
                id
            })
            .collect()
    }

    /// `glDeleteTextures`.
    pub fn delete_textures(&mut self, ids: &[u32]) {
        for id in ids {
            self.textures.remove(id);
            for slot in self.bound_textures.iter_mut() {
                if *slot == *id {
                    *slot = 0;
                }
            }
        }
    }

    /// `glActiveTexture`.
    pub fn active_texture(&mut self, texture: u32) {
        let unit = texture.wrapping_sub(c::TEXTURE0) as usize;
        if unit >= c::TEXTURE_UNITS {
            unsupported("glActiveTexture", format!("unit {texture:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.active_texture = unit;
    }

    /// `glBindTexture`.
    pub fn bind_texture(&mut self, target: u32, id: u32) {
        if target != c::TEXTURE_2D {
            unsupported("glBindTexture", format!("target {target:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if id != 0 && !self.textures.contains_key(&id) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        self.bound_textures[self.active_texture] = id;
    }

    /// `glTexParameteri` / `glTexParameterf`.
    pub fn tex_parameteri(&mut self, target: u32, pname: u32, value: u32) {
        if target != c::TEXTURE_2D {
            unsupported("glTexParameteri", format!("target {target:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        let id = self.bound_textures[self.active_texture];
        let Some(t) = self.textures.get_mut(&id) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        match pname {
            c::TEXTURE_MIN_FILTER => {
                if !matches!(
                    value,
                    c::NEAREST
                        | c::LINEAR
                        | c::NEAREST_MIPMAP_NEAREST
                        | c::LINEAR_MIPMAP_NEAREST
                        | c::NEAREST_MIPMAP_LINEAR
                        | c::LINEAR_MIPMAP_LINEAR
                ) {
                    self.set_error(c::INVALID_ENUM);
                    return;
                }
                t.min_filter = value;
            }
            c::TEXTURE_MAG_FILTER => {
                if !matches!(value, c::NEAREST | c::LINEAR) {
                    self.set_error(c::INVALID_ENUM);
                    return;
                }
                t.mag_filter = value;
            }
            c::TEXTURE_WRAP_S => t.wrap_s = value,
            c::TEXTURE_WRAP_T => t.wrap_t = value,
            other => {
                unsupported("glTexParameteri", format!("pname {other:#x}"));
                self.set_error(c::INVALID_ENUM);
            }
        }
    }

    /// `glTexImage2D` (RGBA/RGB/LUMINANCE/ALPHA + `UNSIGNED_BYTE`).
    #[allow(clippy::too_many_arguments)]
    pub fn tex_image_2d(
        &mut self,
        target: u32,
        level: i32,
        internal: i32,
        width: i32,
        height: i32,
        border: i32,
        format: u32,
        ty: u32,
        pixels: &[u8],
    ) {
        if target != c::TEXTURE_2D {
            unsupported("glTexImage2D", format!("target {target:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if level != 0 {
            // Only level 0 is stored; a mip level above 0 is accepted but the
            // sampler always reads level 0 (reported, not silent).
            diagnostic(
                "glTexImage2D",
                format!("mip level {level} ignored; level 0 is sampled"),
            );
        }
        if border != 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        if ty != c::UNSIGNED_BYTE {
            unsupported("glTexImage2D", format!("pixel type {ty:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if width <= 0 || height <= 0 || width as u32 > c::MAX_DIM || height as u32 > c::MAX_DIM {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        let Some(rgba) = convert_pixels(format, width as u32, height as u32, pixels, self.unpack_alignment)
        else {
            unsupported("glTexImage2D", format!("pixel format {format:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        };
        let id = self.bound_textures[self.active_texture];
        let Some(t) = self.textures.get_mut(&id) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        let _ = internal;
        t.width = width as u32;
        t.height = height as u32;
        t.rgba = rgba;
    }

    /// `glTexSubImage2D`.
    #[allow(clippy::too_many_arguments)]
    pub fn tex_sub_image_2d(
        &mut self,
        target: u32,
        level: i32,
        xoffset: i32,
        yoffset: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
        pixels: &[u8],
    ) {
        if target != c::TEXTURE_2D || ty != c::UNSIGNED_BYTE || level != 0 {
            unsupported(
                "glTexSubImage2D",
                format!("target {target:#x} level {level} type {ty:#x}"),
            );
            self.set_error(c::INVALID_ENUM);
            return;
        }
        let id = self.bound_textures[self.active_texture];
        let Some(t) = self.textures.get_mut(&id) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        if xoffset < 0
            || yoffset < 0
            || width <= 0
            || height <= 0
            || (xoffset as u32 + width as u32) > t.width
            || (yoffset as u32 + height as u32) > t.height
        {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        let Some(src) = convert_pixels(format, width as u32, height as u32, pixels, self.unpack_alignment)
        else {
            unsupported("glTexSubImage2D", format!("pixel format {format:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        };
        for row in 0..height as usize {
            let dst = ((yoffset as usize + row) * t.width as usize + xoffset as usize) * 4;
            let src_row = row * width as usize * 4;
            t.rgba[dst..dst + width as usize * 4]
                .copy_from_slice(&src[src_row..src_row + width as usize * 4]);
        }
    }

    /// `glGenerateMipmap` — accepted; the sampler reads level 0 only.
    pub fn generate_mipmap(&mut self, target: u32) {
        if target != c::TEXTURE_2D {
            self.set_error(c::INVALID_ENUM);
            return;
        }
        diagnostic(
            "glGenerateMipmap",
            "mipmaps are not generated; the sampler filters level 0",
        );
    }

    // -- framebuffers ----------------------------------------------------

    /// `glGenFramebuffers`.
    pub fn gen_framebuffers(&mut self, n: i32) -> Vec<u32> {
        (0..n.max(0))
            .map(|_| {
                let id = self.id();
                self.framebuffers
                    .insert(id, Box::new(Framebuffer::new(0, 0)));
                id
            })
            .collect()
    }

    /// `glDeleteFramebuffers`.
    pub fn delete_framebuffers(&mut self, ids: &[u32]) {
        for id in ids {
            self.framebuffers.remove(id);
            self.fbo_color.remove(id);
            self.fbo_depth.remove(id);
            self.fbo_depth_rb.remove(id);
            if self.framebuffer == *id {
                self.framebuffer = 0;
            }
        }
    }

    /// `glBindFramebuffer`.
    pub fn bind_framebuffer(&mut self, target: u32, id: u32) {
        if target != c::FRAMEBUFFER {
            unsupported("glBindFramebuffer", format!("target {target:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if id != 0 && !self.framebuffers.contains_key(&id) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        self.framebuffer = id;
        self.sync_fbo_from_attachment(id);
    }

    /// Copy the attached colour texture into the FBO's raster target.
    fn sync_fbo_from_attachment(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        let Some(tex_id) = self.fbo_color.get(&id).copied() else {
            return;
        };
        let Some(tex) = self.textures.get(&tex_id) else {
            return;
        };
        if !tex.allocated() {
            return;
        }
        let (w, h, rgba) = (tex.width, tex.height, tex.rgba.clone());
        if let Some(fb) = self.framebuffers.get_mut(&id) {
            fb.resize(w, h);
            if fb.color.len() == rgba.len() {
                fb.color.copy_from_slice(&rgba);
            }
        }
    }

    /// Copy the FBO's raster target back into its colour texture.
    pub fn flush_fbo_to_attachment(&mut self, id: u32) {
        if id == 0 {
            return;
        }
        let Some(tex_id) = self.fbo_color.get(&id).copied() else {
            return;
        };
        let Some(fb) = self.framebuffers.get(&id) else {
            return;
        };
        let (w, h, color) = (fb.width, fb.height, fb.color.clone());
        if let Some(tex) = self.textures.get_mut(&tex_id) {
            if tex.width == w && tex.height == h {
                tex.rgba = color;
            }
        }
    }

    /// `glFramebufferTexture2D`.
    pub fn framebuffer_texture_2d(&mut self, target: u32, attachment: u32, tex: u32, level: i32) {
        if target != c::FRAMEBUFFER || level != 0 {
            unsupported(
                "glFramebufferTexture2D",
                format!("target {target:#x} level {level}"),
            );
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if tex != 0 && !self.textures.contains_key(&tex) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        let fbo = self.framebuffer;
        if fbo == 0 {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        match attachment {
            c::COLOR_ATTACHMENT0 => {
                if tex == 0 {
                    self.fbo_color.remove(&fbo);
                } else {
                    self.fbo_color.insert(fbo, tex);
                }
            }
            c::DEPTH_ATTACHMENT => {
                if tex == 0 {
                    self.fbo_depth.remove(&fbo);
                } else {
                    self.fbo_depth.insert(fbo, tex);
                }
            }
            other => {
                unsupported("glFramebufferTexture2D", format!("attachment {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                return;
            }
        }
        self.sync_fbo_from_attachment(fbo);
    }

    /// `glFramebufferRenderbuffer`.
    pub fn framebuffer_renderbuffer(&mut self, target: u32, attachment: u32, rb: u32) {
        if target != c::FRAMEBUFFER {
            self.set_error(c::INVALID_ENUM);
            return;
        }
        let fbo = self.framebuffer;
        if fbo == 0 {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        if attachment == c::DEPTH_ATTACHMENT {
            if rb == 0 {
                self.fbo_depth_rb.remove(&fbo);
            } else {
                self.fbo_depth_rb.insert(fbo, rb);
            }
            return;
        }
        unsupported(
            "glFramebufferRenderbuffer",
            format!("attachment {attachment:#x} is not backed by a renderbuffer"),
        );
        self.set_error(c::INVALID_ENUM);
    }

    /// `glCheckFramebufferStatus`.
    #[must_use]
    pub fn check_framebuffer_status(&self, target: u32) -> u32 {
        if target != c::FRAMEBUFFER {
            return c::NONE;
        }
        let id = self.framebuffer;
        if id == 0 || !self.framebuffers.contains_key(&id) {
            return c::FRAMEBUFFER_COMPLETE;
        }
        match self.fbo_color.get(&id) {
            None => c::FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT,
            Some(tex) => match self.textures.get(tex) {
                Some(t) if t.allocated() => c::FRAMEBUFFER_COMPLETE,
                _ => c::FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
            },
        }
    }

    /// `glGenRenderbuffers`.
    pub fn gen_renderbuffers(&mut self, n: i32) -> Vec<u32> {
        (0..n.max(0))
            .map(|_| {
                let id = self.id();
                self.renderbuffers.insert(
                    id,
                    Renderbuffer {
                        width: 0,
                        height: 0,
                    },
                );
                id
            })
            .collect()
    }

    /// `glDeleteRenderbuffers`.
    pub fn delete_renderbuffers(&mut self, ids: &[u32]) {
        for id in ids {
            self.renderbuffers.remove(id);
            for (fbo, rb) in self.fbo_depth_rb.clone() {
                if rb == *id {
                    self.fbo_depth_rb.remove(&fbo);
                }
            }
            if self.renderbuffer == *id {
                self.renderbuffer = 0;
            }
        }
    }

    /// `glBindRenderbuffer`.
    pub fn bind_renderbuffer(&mut self, target: u32, id: u32) {
        if target != c::RENDERBUFFER {
            unsupported("glBindRenderbuffer", format!("target {target:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        if id != 0 && !self.renderbuffers.contains_key(&id) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        self.renderbuffer = id;
    }

    /// `glRenderbufferStorage`.
    pub fn renderbuffer_storage(&mut self, target: u32, _format: u32, width: i32, height: i32) {
        if target != c::RENDERBUFFER || width < 0 || height < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        let id = self.renderbuffer;
        match self.renderbuffers.get_mut(&id) {
            Some(rb) => {
                rb.width = width as u32;
                rb.height = height as u32;
            }
            None => self.set_error(c::INVALID_OPERATION),
        }
    }

    // -- fixed-function state -------------------------------------------

    /// `glViewport`.
    pub fn viewport(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if w < 0 || h < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        self.viewport = (x, y, w as u32, h as u32);
    }

    /// `glScissor`.
    pub fn scissor(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if w < 0 || h < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        self.scissor = (x, y, w as u32, h as u32);
    }

    /// `glEnable`.
    pub fn enable(&mut self, cap: u32) {
        match cap {
            c::DEPTH_TEST => self.depth_test = true,
            c::BLEND => self.blend = true,
            c::CULL_FACE => self.cull = true,
            c::SCISSOR_TEST => self.scissor_test = true,
            c::DITHER => {}
            other => {
                unsupported("glEnable", format!("capability {other:#x}"));
                self.set_error(c::INVALID_ENUM);
            }
        }
    }

    /// `glDisable`.
    pub fn disable(&mut self, cap: u32) {
        match cap {
            c::DEPTH_TEST => self.depth_test = false,
            c::BLEND => self.blend = false,
            c::CULL_FACE => self.cull = false,
            c::SCISSOR_TEST => self.scissor_test = false,
            c::DITHER => {}
            other => {
                unsupported("glDisable", format!("capability {other:#x}"));
                self.set_error(c::INVALID_ENUM);
            }
        }
    }

    /// `glBlendFunc`.
    pub fn blend_func(&mut self, src: u32, dst: u32) {
        if blend_factor(src).is_none() || blend_factor(dst).is_none() {
            unsupported("glBlendFunc", format!("factors {src:#x}/{dst:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.blend_rgb = (src, dst);
        self.blend_alpha = (src, dst);
    }

    /// `glBlendFuncSeparate`.
    pub fn blend_func_separate(&mut self, src_rgb: u32, dst_rgb: u32, src_a: u32, dst_a: u32) {
        if [src_rgb, dst_rgb, src_a, dst_a]
            .iter()
            .any(|f| blend_factor(*f).is_none())
        {
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.blend_rgb = (src_rgb, dst_rgb);
        self.blend_alpha = (src_a, dst_a);
    }

    /// `glBlendEquation` / `glBlendEquationSeparate` — `FUNC_ADD` only.
    pub fn blend_equation(&mut self, mode: u32) {
        if mode == c::FUNC_ADD {
            return;
        }
        unsupported(
            "glBlendEquation",
            format!("mode {mode:#x} (only GL_FUNC_ADD is implemented)"),
        );
        self.set_error(c::INVALID_OPERATION);
    }

    /// `glDepthFunc`.
    pub fn depth_func(&mut self, func: u32) {
        if depth_func(func).is_none() {
            unsupported("glDepthFunc", format!("func {func:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.depth_func = func;
    }

    /// `glDepthMask`.
    pub fn depth_mask(&mut self, flag: bool) {
        self.depth_mask = flag;
    }

    /// `glColorMask`.
    pub fn color_mask(&mut self, r: bool, g: bool, b: bool, a: bool) {
        self.color_mask = [r, g, b, a];
    }

    /// `glClearColor`.
    pub fn clear_color(&mut self, r: f32, g: f32, b: f32, a: f32) {
        self.clear_color = [r, g, b, a];
    }

    /// `glClearDepthf`.
    pub fn clear_depthf(&mut self, d: f32) {
        self.clear_depth = d.clamp(0.0, 1.0);
    }

    /// `glClearStencil` — accepted (there is no stencil buffer).
    pub fn clear_stencil(&mut self, _v: i32) {
        diagnostic("glClearStencil", "no stencil buffer; `STENCIL_BUFFER_BIT` is ignored");
    }

    /// `glCullFace`.
    pub fn cull_face(&mut self, mode: u32) {
        if !matches!(mode, c::FRONT | c::BACK | c::FRONT_AND_BACK) {
            self.set_error(c::INVALID_ENUM);
            return;
        }
        self.cull_face = mode;
    }

    /// `glFrontFace`.
    pub fn front_face(&mut self, mode: u32) {
        match mode {
            c::CW => self.front_ccw = false,
            c::CCW => self.front_ccw = true,
            _ => self.set_error(c::INVALID_ENUM),
        }
    }

    /// `glPixelStorei`.
    pub fn pixel_storei(&mut self, pname: u32, value: i32) {
        match pname {
            c::UNPACK_ALIGNMENT => {
                if !matches!(value, 1 | 2 | 4 | 8) {
                    self.set_error(c::INVALID_VALUE);
                    return;
                }
                self.unpack_alignment = value;
            }
            c::PACK_ALIGNMENT => {
                if !matches!(value, 1 | 2 | 4 | 8) {
                    self.set_error(c::INVALID_VALUE);
                }
            }
            other => {
                unsupported("glPixelStorei", format!("pname {other:#x}"));
                self.set_error(c::INVALID_ENUM);
            }
        }
    }

    /// `glLineWidth` — accepted for the GLES2 `1.0` width only.
    pub fn line_width(&mut self, width: f32) {
        if width <= 0.0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        if (width - 1.0).abs() > f32::EPSILON {
            diagnostic("glLineWidth", format!("width {width} is rasterised as 1.0"));
        }
        self.line_width = width;
    }

    /// `glStencil*` — no stencil buffer exists; accepted with a diagnostic.
    pub fn stencil_noop(&mut self, call: &str) {
        diagnostic(call, "no stencil buffer; the stencil state is recorded only");
    }

    // -- target resolution ----------------------------------------------

    /// The framebuffer rasterisation writes to.
    fn raster_state(&self, fb_width: u32, fb_height: u32) -> RasterState {
        let viewport = if self.viewport.2 == 0 || self.viewport.3 == 0 {
            (0, 0, fb_width, fb_height)
        } else {
            self.viewport
        };
        RasterState {
            viewport,
            scissor: if self.scissor_test {
                Some(self.scissor)
            } else {
                None
            },
            blend: if self.blend {
                Some(Blend {
                    src_rgb: blend_factor(self.blend_rgb.0).unwrap_or(BlendFactor::One),
                    dst_rgb: blend_factor(self.blend_rgb.1).unwrap_or(BlendFactor::Zero),
                    src_alpha: blend_factor(self.blend_alpha.0).unwrap_or(BlendFactor::One),
                    dst_alpha: blend_factor(self.blend_alpha.1).unwrap_or(BlendFactor::Zero),
                })
            } else {
                None
            },
            depth_test: self.depth_test,
            depth_func: depth_func(self.depth_func).unwrap_or(DepthFunc::Less),
            depth_mask: self.depth_mask,
            color_mask: self.color_mask,
            cull: if self.cull {
                Some(match self.cull_face {
                    c::FRONT => Cull::Front,
                    c::FRONT_AND_BACK => Cull::FrontAndBack,
                    _ => Cull::Back,
                })
            } else {
                None
            },
            front_ccw: self.front_ccw,
        }
    }

    // -- clear / read ----------------------------------------------------

    /// `glClear`.
    pub fn clear(&mut self, surface: &mut Framebuffer, mask: u32) {
        let color = self.clear_color;
        let px = [
            to_u8(color[0]),
            to_u8(color[1]),
            to_u8(color[2]),
            to_u8(color[3]),
        ];
        let depth = self.clear_depth;
        let depth_mask = self.depth_mask;
        let color_mask = self.color_mask;
        let scissor = if self.scissor_test {
            Some(self.scissor)
        } else {
            None
        };
        let fbo = self.framebuffer;
        if fbo != 0 && !self.framebuffers.contains_key(&fbo) {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        let target: &mut Framebuffer = if fbo != 0 {
            match self.framebuffers.get_mut(&fbo) {
                Some(fb) => fb,
                None => {
                    self.set_error(c::INVALID_OPERATION);
                    return;
                }
            }
        } else {
            surface
        };
        if target.width == 0 || target.height == 0 {
            self.set_error(c::INVALID_FRAMEBUFFER_OPERATION);
            return;
        }
        let rect = scissor.map_or((0, 0, target.width as i32, target.height as i32), |(x, y, w, h)| {
            (x, y, x + w as i32, y + h as i32)
        });
        if mask & c::COLOR_BUFFER_BIT != 0 {
            if color_mask.iter().all(|m| *m) {
                target.fill_rect(rect, px);
            } else {
                let (x0, y0, x1, y1) = rect;
                for y in y0.max(0)..y1.min(target.height as i32) {
                    for x in x0.max(0)..x1.min(target.width as i32) {
                        let mut cur = target.pixel(x as u32, y as u32);
                        for k in 0..4 {
                            if color_mask[k] {
                                cur[k] = px[k];
                            }
                        }
                        target.set_pixel(x as u32, y as u32, cur);
                    }
                }
            }
        }
        if mask & c::DEPTH_BUFFER_BIT != 0 && depth_mask {
            if let Some(d) = target.depth.as_mut() {
                let (x0, y0, x1, y1) = rect;
                for y in y0.max(0)..y1.min(target.height as i32) {
                    let row = y as usize * target.width as usize;
                    for x in x0.max(0)..x1.min(target.width as i32) {
                        d[row + x as usize] = depth;
                    }
                }
            }
        }
        if mask & c::STENCIL_BUFFER_BIT != 0 {
            diagnostic("glClear", "no stencil buffer; STENCIL_BUFFER_BIT ignored");
        }
        self.flush_fbo_to_attachment(fbo);
    }

    /// `glReadPixels` — RGBA/UNSIGNED_BYTE only.
    #[allow(clippy::too_many_arguments)]
    pub fn read_pixels(
        &mut self,
        surface: &Framebuffer,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        format: u32,
        ty: u32,
    ) -> Vec<u8> {
        if format != c::RGBA || ty != c::UNSIGNED_BYTE {
            unsupported(
                "glReadPixels",
                format!("format {format:#x} type {ty:#x} (RGBA/UNSIGNED_BYTE only)"),
            );
            self.set_error(c::INVALID_OPERATION);
            return Vec::new();
        }
        if width <= 0 || height <= 0 {
            self.set_error(c::INVALID_VALUE);
            return Vec::new();
        }
        let fbo = self.framebuffer;
        let fb = if fbo != 0 {
            match self.framebuffers.get(&fbo) {
                Some(fb) => &**fb,
                None => {
                    self.set_error(c::INVALID_OPERATION);
                    return Vec::new();
                }
            }
        } else {
            surface
        };
        let mut out = vec![0u8; width as usize * height as usize * 4];
        for row in 0..height {
            for col in 0..width {
                let (sx, sy) = (x + col, y + row);
                // GL's window origin is the bottom-left, the framebuffer's is
                // the top-left, so `y` counts up from the last stored row.
                let sy = fb.height as i32 - 1 - sy;
                let px = if sx < 0 || sy < 0 || sx >= fb.width as i32 || sy >= fb.height as i32 {
                    [0, 0, 0, 0]
                } else {
                    fb.pixel(sx as u32, sy as u32)
                };
                let i = (row as usize * width as usize + col as usize) * 4;
                out[i..i + 4].copy_from_slice(&px);
            }
        }
        out
    }

    /// `glGetIntegerv` / `glGetIntegeri_v` for the implemented pnames.
    pub fn get_integer_v(&mut self, pname: u32) -> Vec<i32> {
        let v = match pname {
            c::MAX_TEXTURE_SIZE | c::MAX_RENDERBUFFER_SIZE => vec![c::MAX_DIM as i32],
            c::MAX_VIEWPORT_DIMS => vec![c::MAX_DIM as i32, c::MAX_DIM as i32],
            c::MAX_VERTEX_ATTRIBS => vec![c::MAX_ATTRIBS as i32],
            c::MAX_TEXTURE_IMAGE_UNITS | c::MAX_COMBINED_TEXTURE_IMAGE_UNITS => {
                vec![c::TEXTURE_UNITS as i32]
            }
            c::MAX_VERTEX_UNIFORM_VECTORS => vec![256],
            c::MAX_FRAGMENT_UNIFORM_VECTORS => vec![224],
            c::MAX_VARYING_VECTORS => vec![8],
            c::MAX_CUBE_MAP_TEXTURE_SIZE => vec![0],
            c::NUM_COMPRESSED_TEXTURE_FORMATS => vec![0],
            c::COMPRESSED_TEXTURE_FORMATS => vec![],
            c::VIEWPORT => vec![
                self.viewport.0,
                self.viewport.1,
                self.viewport.2 as i32,
                self.viewport.3 as i32,
            ],
            c::CURRENT_PROGRAM => vec![self.current_program as i32],
            c::ARRAY_BUFFER_BINDING => vec![self.array_buffer as i32],
            c::ELEMENT_ARRAY_BUFFER_BINDING => vec![self.element_buffer as i32],
            c::FRAMEBUFFER_BINDING => vec![self.framebuffer as i32],
            c::RENDERBUFFER_BINDING => vec![self.renderbuffer as i32],
            c::TEXTURE_BINDING_2D => vec![self.bound_textures[self.active_texture] as i32],
            c::ACTIVE_TEXTURE => vec![c::TEXTURE0 as i32 + self.active_texture as i32],
            c::DEPTH_WRITEMASK => vec![i32::from(self.depth_mask)],
            c::UNPACK_ALIGNMENT => vec![self.unpack_alignment],
            c::PACK_ALIGNMENT => vec![4],
            other => {
                unsupported("glGetIntegerv", format!("pname {other:#x}"));
                self.set_error(c::INVALID_ENUM);
                vec![]
            }
        };
        v
    }

    /// `glGetString`.
    #[must_use]
    pub fn get_string(&self, name: u32) -> Option<&'static str> {
        match name {
            c::VENDOR => Some("ACOS"),
            c::RENDERER => Some("ACOS software GLES2 (acos-gles)"),
            c::VERSION => Some("OpenGL ES 2.0 ACOS-1"),
            c::SHADING_LANGUAGE_VERSION => Some("OpenGL ES GLSL ES 1.00 ACOS-1"),
            c::EXTENSIONS => Some("GL_OES_rgb8_rgba8 GL_OES_depth24 GL_OES_fbo_render_mipmap"),
            _ => None,
        }
    }

    // -- draws -----------------------------------------------------------

    /// `glDrawArrays`.
    pub fn draw_arrays(&mut self, surface: &mut Framebuffer, mode: u32, first: i32, count: i32) {
        if count < 0 || first < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        let indices: Vec<u32> = (first..first + count).map(|i| i as u32).collect();
        self.draw_indexed(surface, mode, &indices);
    }

    /// `glDrawElements`.
    pub fn draw_elements(
        &mut self,
        surface: &mut Framebuffer,
        mode: u32,
        count: i32,
        ty: u32,
        offset: i32,
    ) {
        if count < 0 || offset < 0 {
            self.set_error(c::INVALID_VALUE);
            return;
        }
        if !matches!(ty, c::UNSIGNED_SHORT | c::UNSIGNED_BYTE | c::UNSIGNED_INT) {
            unsupported("glDrawElements", format!("index type {ty:#x}"));
            self.set_error(c::INVALID_ENUM);
            return;
        }
        let Some(buf) = self.buffers.get(&self.element_buffer) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        let stride = match ty {
            c::UNSIGNED_BYTE => 1,
            c::UNSIGNED_SHORT => 2,
            _ => 4,
        };
        let need = offset as usize + count as usize * stride;
        if need > buf.len() {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        let mut indices = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let at = offset as usize + i * stride;
            indices.push(match ty {
                c::UNSIGNED_BYTE => u32::from(buf[at]),
                c::UNSIGNED_SHORT => u32::from(u16::from_le_bytes([buf[at], buf[at + 1]])),
                _ => u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]),
            });
        }
        self.draw_indexed(surface, mode, &indices);
    }

    fn draw_indexed(&mut self, surface: &mut Framebuffer, mode: u32, indices: &[u32]) {
        if !matches!(mode, c::TRIANGLES | c::TRIANGLE_STRIP | c::TRIANGLE_FAN) {
            unsupported(
                "glDrawArrays/glDrawElements",
                format!("primitive mode {mode:#x} (only TRIANGLES/STRIP/FAN are rasterised)"),
            );
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        let program_id = self.current_program;
        if program_id == 0 {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        let Some(program) = self.programs.get(&program_id) else {
            self.set_error(c::INVALID_OPERATION);
            return;
        };
        if !program.linked {
            self.set_error(c::INVALID_OPERATION);
            return;
        }
        if indices.is_empty() {
            return;
        }
        let uniforms: Vec<(String, Value)> = self
            .uniforms
            .get(&program_id)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let fs_varyings: Vec<String> = program.fs_unit.interface().varyings.clone();
        let attrib_names: Vec<String> = program.vs_unit.interface().attributes.clone();
        let attrib_locations: Vec<Option<u32>> = attrib_names
            .iter()
            .map(|n| program.attrib_location(n))
            .collect();

        // ---- vertex stage (immutable borrows only) ----
        let mut cache: HashMap<u32, VertexOut> = HashMap::new();
        let mut vertices: Vec<VertexOut> = Vec::with_capacity(indices.len());
        for index in indices {
            if let Some(v) = cache.get(index) {
                vertices.push(v.clone());
                continue;
            }
            let mut attrs: Vec<(String, Value)> = Vec::with_capacity(attrib_names.len());
            for (name, loc) in attrib_names.iter().zip(attrib_locations.iter()) {
                let Some(loc) = *loc else { continue };
                let value = if self.attrib_enabled.get(loc as usize).copied().unwrap_or(false) {
                    match self.attrib_arrays.get(&loc) {
                        Some(array) => match self.buffers.get(&array.buffer) {
                            Some(buf) => read_attribute(buf, *index, array),
                            None => None,
                        },
                        None => None,
                    }
                } else {
                    Some(Value::vec4(
                        self.attrib_generic[loc as usize][0],
                        self.attrib_generic[loc as usize][1],
                        self.attrib_generic[loc as usize][2],
                        self.attrib_generic[loc as usize][3],
                    ))
                };
                match value {
                    Some(v) => attrs.push((name.clone(), v)),
                    None => {
                        unsupported(
                            "glDrawArrays/glDrawElements",
                            format!("attribute {name:?} has no bound array data"),
                        );
                        self.set_error(c::INVALID_OPERATION);
                        return;
                    }
                }
            }
            match glsl::run_vertex(&program.vs_unit, &uniforms, &attrs, &fs_varyings) {
                Ok(v) => {
                    cache.insert(*index, v.clone());
                    vertices.push(v);
                }
                Err(e) => {
                    unsupported("glDrawArrays", format!("vertex shader: {e}"));
                    self.set_error(c::INVALID_OPERATION);
                    return;
                }
            }
        }

        let triangles = assemble(mode, &vertices);
        if triangles.is_empty() {
            return;
        }

        // ---- target + fragment stage ----
        //
        // `target` mutably borrows one map entry while `program`, `textures`
        // and `uniforms` are borrowed immutably: separate fields of `self`, so
        // the borrow checker accepts the split.
        let fbo = self.framebuffer;
        let (target_w, target_h) = if fbo != 0 {
            match self.framebuffers.get(&fbo) {
                Some(fb) => (fb.width, fb.height),
                None => {
                    self.set_error(c::INVALID_OPERATION);
                    return;
                }
            }
        } else {
            (surface.width, surface.height)
        };
        if target_w == 0 || target_h == 0 {
            self.set_error(c::INVALID_FRAMEBUFFER_OPERATION);
            return;
        }
        let state = self.raster_state(target_w, target_h);
        let (textures, bound) = (&self.textures, self.bound_textures);
        let sampler = Sampler { textures, bound };
        let target: &mut Framebuffer = if fbo != 0 {
            match self.framebuffers.get_mut(&fbo) {
                Some(fb) => fb,
                None => {
                    self.set_error(c::INVALID_OPERATION);
                    return;
                }
            }
        } else {
            surface
        };
        let mut errors: Vec<String> = Vec::new();
        for (a, b, cc) in triangles {
            // Anything at/behind the eye cannot be projected; GL clips it.
            if a.clip[3] <= 0.0 || b.clip[3] <= 0.0 || cc.clip[3] <= 0.0 {
                continue;
            }
            let screen = [
                project(a.clip, a.varyings, &state),
                project(b.clip, b.varyings, &state),
                project(cc.clip, cc.varyings, &state),
            ];
            crate::raster::triangle(
                target,
                &state,
                [&screen[0], &screen[1], &screen[2]],
                |varyings, coord| {
                    match glsl::run_fragment(
                        &program.fs_unit,
                        &uniforms,
                        &fs_varyings,
                        varyings,
                        coord,
                        &sampler,
                    ) {
                        Ok(v) => v,
                        Err(e) => {
                            errors.push(format!("fragment shader: {e}"));
                            None
                        }
                    }
                },
            );
        }
        if let Some(e) = errors.first() {
            unsupported("glDrawArrays", e.clone());
            self.set_error(c::INVALID_OPERATION);
        }
        self.flush_fbo_to_attachment(fbo);
    }

    /// Number of live objects (diagnostics/tests).
    #[must_use]
    pub fn object_counts(&self) -> (usize, usize, usize, usize, usize) {
        (
            self.buffers.len(),
            self.textures.len(),
            self.shaders.len(),
            self.programs.len(),
            self.framebuffers.len(),
        )
    }
}

fn assemble(mode: u32, vertices: &[VertexOut]) -> Vec<(VertexOut, VertexOut, VertexOut)> {
    let mut out = Vec::new();
    match mode {
        c::TRIANGLES => {
            for chunk in vertices.chunks_exact(3) {
                out.push((chunk[0].clone(), chunk[1].clone(), chunk[2].clone()));
            }
        }
        c::TRIANGLE_STRIP => {
            for i in 0..vertices.len().saturating_sub(2) {
                let (a, b, cc) = (i, i + 1, i + 2);
                if i % 2 == 0 {
                    out.push((
                        vertices[a].clone(),
                        vertices[b].clone(),
                        vertices[cc].clone(),
                    ));
                } else {
                    out.push((
                        vertices[b].clone(),
                        vertices[a].clone(),
                        vertices[cc].clone(),
                    ));
                }
            }
        }
        c::TRIANGLE_FAN => {
            for i in 1..vertices.len().saturating_sub(1) {
                out.push((
                    vertices[0].clone(),
                    vertices[i].clone(),
                    vertices[i + 1].clone(),
                ));
            }
        }
        _ => {}
    }
    out
}

/// Sample bound textures for `texture2D`.
struct Sampler<'a> {
    textures: &'a HashMap<u32, Texture>,
    bound: [u32; c::TEXTURE_UNITS],
}

impl TextureLookup for Sampler<'_> {
    fn sample(&self, unit: i32, u: f32, v: f32) -> [f32; 4] {
        let unit = unit.clamp(0, c::TEXTURE_UNITS as i32 - 1) as usize;
        let Some(tex) = self.textures.get(&self.bound[unit]) else {
            return [0.0, 0.0, 0.0, 1.0];
        };
        if tex.width == 0 || tex.height == 0 {
            return [0.0, 0.0, 0.0, 1.0];
        }
        let wrap = |c: f32, mode: u32| -> f32 {
            match mode {
                c::CLAMP_TO_EDGE => c.clamp(0.0, 1.0),
                c::MIRRORED_REPEAT => {
                    let t = c - 2.0 * (c * 0.5).floor();
                    if t > 1.0 {
                        2.0 - t
                    } else {
                        t.abs()
                    }
                }
                _ => c - c.floor(),
            }
        };
        let u = wrap(u, tex.wrap_s);
        let v = wrap(v, tex.wrap_t);
        let linear = matches!(
            tex.mag_filter,
            c::LINEAR
        ) || matches!(
            tex.min_filter,
            c::LINEAR
                | c::LINEAR_MIPMAP_NEAREST
                | c::LINEAR_MIPMAP_LINEAR
                | c::NEAREST_MIPMAP_LINEAR
        );
        let fetch = |x: i32, y: i32| -> [f32; 4] {
            let x = x.clamp(0, tex.width as i32 - 1) as usize;
            let y = y.clamp(0, tex.height as i32 - 1) as usize;
            let i = (y * tex.width as usize + x) * 4;
            [
                f32::from(tex.rgba[i]) / 255.0,
                f32::from(tex.rgba[i + 1]) / 255.0,
                f32::from(tex.rgba[i + 2]) / 255.0,
                f32::from(tex.rgba[i + 3]) / 255.0,
            ]
        };
        if !linear {
            let x = (u * tex.width as f32) as i32;
            let y = (v * tex.height as f32) as i32;
            return fetch(x, y);
        }
        let fx = u * tex.width as f32 - 0.5;
        let fy = v * tex.height as f32 - 0.5;
        let x0 = fx.floor() as i32;
        let y0 = fy.floor() as i32;
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let p00 = fetch(x0, y0);
        let p10 = fetch(x0 + 1, y0);
        let p01 = fetch(x0, y0 + 1);
        let p11 = fetch(x0 + 1, y0 + 1);
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            let top = p00[k] * (1.0 - tx) + p10[k] * tx;
            let bottom = p01[k] * (1.0 - tx) + p11[k] * tx;
            out[k] = top * (1.0 - ty) + bottom * ty;
        }
        out
    }
}

fn read_attribute(buf: &[u8], index: u32, array: &AttribArray) -> Option<Value> {
    let comp_size = match array.ty {
        c::BYTE | c::UNSIGNED_BYTE => 1usize,
        c::SHORT | c::UNSIGNED_SHORT => 2,
        _ => 4,
    };
    let stride = if array.stride > 0 {
        array.stride as usize
    } else {
        comp_size * array.size as usize
    };
    let base = array.offset as usize + stride * index as usize;
    let need = base + comp_size * array.size as usize;
    if need > buf.len() {
        return None;
    }
    let mut out = [0.0f32, 0.0, 0.0, 1.0];
    for (i, slot) in out.iter_mut().enumerate().take(array.size as usize) {
        let at = base + i * comp_size;
        *slot = match array.ty {
            c::BYTE => {
                let raw = buf[at] as i8 as f32 / 127.0;
                if array.normalized {
                    raw.max(-1.0)
                } else {
                    f32::from(buf[at] as i8)
                }
            }
            c::UNSIGNED_BYTE => {
                let v = f32::from(buf[at]);
                if array.normalized {
                    v / 255.0
                } else {
                    v
                }
            }
            c::SHORT => {
                let v = i16::from_le_bytes([buf[at], buf[at + 1]]);
                if array.normalized {
                    (f32::from(v) / 32767.0).max(-1.0)
                } else {
                    f32::from(v)
                }
            }
            c::UNSIGNED_SHORT => {
                let v = u16::from_le_bytes([buf[at], buf[at + 1]]);
                if array.normalized {
                    f32::from(v) / 65535.0
                } else {
                    f32::from(v)
                }
            }
            c::FIXED => i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
                as f32
                / 65536.0,
            _ => f32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]),
        };
    }
    Some(if array.size == 1 {
        Value::F(out[0])
    } else {
        Value::V(out, array.size as usize)
    })
}

fn convert_pixels(
    format: u32,
    width: u32,
    height: u32,
    pixels: &[u8],
    alignment: i32,
) -> Option<Vec<u8>> {
    let comps = match format {
        c::RGBA => 4usize,
        c::RGB => 3,
        c::LUMINANCE_ALPHA => 2,
        c::LUMINANCE | c::ALPHA => 1,
        _ => return None,
    };
    let row_bytes = width as usize * comps;
    let align = alignment.max(1) as usize;
    let padded = row_bytes.div_ceil(align) * align;
    if pixels.len() < padded * height as usize {
        // A guest that uploads a tightly packed buffer with a larger
        // UNPACK_ALIGNMENT is common; fall back to the tight row stride.
        if pixels.len() < row_bytes * height as usize {
            return None;
        }
    }
    let stride = if pixels.len() >= padded * height as usize {
        padded
    } else {
        row_bytes
    };
    let mut out = vec![0u8; width as usize * height as usize * 4];
    for row in 0..height as usize {
        for col in 0..width as usize {
            let src = row * stride + col * comps;
            let dst = (row * width as usize + col) * 4;
            match format {
                c::RGBA => out[dst..dst + 4].copy_from_slice(&pixels[src..src + 4]),
                c::RGB => {
                    out[dst] = pixels[src];
                    out[dst + 1] = pixels[src + 1];
                    out[dst + 2] = pixels[src + 2];
                    out[dst + 3] = 255;
                }
                c::LUMINANCE_ALPHA => {
                    let l = pixels[src];
                    out[dst] = l;
                    out[dst + 1] = l;
                    out[dst + 2] = l;
                    out[dst + 3] = pixels[src + 1];
                }
                c::LUMINANCE => {
                    let l = pixels[src];
                    out[dst] = l;
                    out[dst + 1] = l;
                    out[dst + 2] = l;
                    out[dst + 3] = 255;
                }
                _ => {
                    out[dst] = 0;
                    out[dst + 1] = 0;
                    out[dst + 2] = 0;
                    out[dst + 3] = pixels[src];
                }
            }
        }
    }
    Some(out)
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn blend_factor(v: u32) -> Option<BlendFactor> {
    Some(match v {
        c::ZERO => BlendFactor::Zero,
        c::ONE => BlendFactor::One,
        c::SRC_COLOR => BlendFactor::SrcColor,
        c::ONE_MINUS_SRC_COLOR => BlendFactor::OneMinusSrcColor,
        c::SRC_ALPHA => BlendFactor::SrcAlpha,
        c::ONE_MINUS_SRC_ALPHA => BlendFactor::OneMinusSrcAlpha,
        c::DST_ALPHA => BlendFactor::DstAlpha,
        c::ONE_MINUS_DST_ALPHA => BlendFactor::OneMinusDstAlpha,
        c::DST_COLOR => BlendFactor::DstColor,
        c::ONE_MINUS_DST_COLOR => BlendFactor::OneMinusDstColor,
        c::SRC_ALPHA_SATURATE => BlendFactor::SrcAlphaSaturate,
        _ => return None,
    })
}

fn depth_func(v: u32) -> Option<DepthFunc> {
    Some(match v {
        c::NEVER => DepthFunc::Never,
        c::LESS => DepthFunc::Less,
        c::EQUAL => DepthFunc::Equal,
        c::LEQUAL => DepthFunc::Lequal,
        c::GREATER => DepthFunc::Greater,
        c::NOTEQUAL => DepthFunc::Notequal,
        c::GEQUAL => DepthFunc::Gequal,
        c::ALWAYS => DepthFunc::Always,
        _ => return None,
    })
}
