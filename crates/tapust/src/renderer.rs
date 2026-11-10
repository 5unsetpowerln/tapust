//! OpenGL 3.3 renderer.
//!
//! Two draw calls per frame:
//! 1. Backgrounds: one full-window triangle whose fragment shader looks up
//!    the cell under each pixel in a `cols x rows` RGBA texture.
//! 2. Foreground: instanced quads for glyphs (from one RGBA atlas) and solid
//!    rectangles (cursor, underlines, box-drawing lines).
//!
//! Only dirty frame rows rebuild their background texels and instances.

use std::collections::HashMap;

use glow::HasContext;

use crate::font::{Face, Fonts, face_for};
use crate::frame::{BOLD, Frame, FrameCell, ITALIC, STRIKE, UNDERLINE, WIDE};

const ATLAS_SIZE: i32 = 2048;
const FLAG_COLORED: u32 = 1;
const FLAG_RECT: u32 = 2;

#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct Instance {
    cell: [u16; 2],
    offset: [i16; 2],
    size: [u16; 2],
    uv: [u16; 2],
    color: [u8; 4],
    flags: u32,
}

#[derive(Clone, Copy)]
struct Glyph {
    uv: [u16; 2],
    size: [u16; 2],
    offset: [i16; 2],
    colored: bool,
}

/// Shelf packer for the glyph atlas.
struct Atlas {
    texture: glow::Texture,
    x: i32,
    y: i32,
    shelf_height: i32,
}

impl Atlas {
    fn reserve(&mut self, w: i32, h: i32) -> Option<(i32, i32)> {
        if w > ATLAS_SIZE || h > ATLAS_SIZE {
            return None;
        }
        if self.x + w > ATLAS_SIZE {
            self.y += self.shelf_height + 1;
            self.x = 0;
            self.shelf_height = 0;
        }
        if self.y + h > ATLAS_SIZE {
            return None;
        }
        let pos = (self.x, self.y);
        self.x += w + 1;
        self.shelf_height = self.shelf_height.max(h);
        Some(pos)
    }
}

struct Uniforms {
    cell: glow::UniformLocation,
    pad: glow::UniformLocation,
    viewport: glow::UniformLocation,
}

pub struct Renderer {
    gl: glow::Context,
    pub fonts: Fonts,
    bg_program: glow::Program,
    bg_uniforms: Uniforms,
    bg_default: glow::UniformLocation,
    bg_texture: glow::Texture,
    bg_size: (u16, u16),
    bg_row: Vec<[u8; 4]>,
    fg_program: glow::Program,
    fg_uniforms: Uniforms,
    empty_vao: glow::VertexArray,
    fg_vao: glow::VertexArray,
    fg_buffer: glow::Buffer,
    fg_capacity: usize,
    atlas: Atlas,
    ascii: Box<[[Option<Glyph>; 128]; 4]>,
    glyphs: HashMap<(u32, Face), Option<Glyph>>,
    rows: Vec<Vec<Instance>>,
    upload: Vec<Instance>,
    viewport: (u32, u32),
    padding: (f32, f32),
}

const BG_VS: &str = r#"#version 330 core
void main() {
    vec2 p = vec2((gl_VertexID << 1) & 2, gl_VertexID & 2);
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const BG_FS: &str = r#"#version 330 core
uniform sampler2D u_bg;
uniform vec2 u_cell;
uniform vec2 u_pad;
uniform vec2 u_viewport;
uniform vec4 u_default_bg;
out vec4 frag;
void main() {
    vec2 px = vec2(gl_FragCoord.x, u_viewport.y - gl_FragCoord.y);
    ivec2 cell = ivec2(floor((px - u_pad) / u_cell));
    ivec2 size = textureSize(u_bg, 0);
    if (any(lessThan(cell, ivec2(0))) || any(greaterThanEqual(cell, size))) {
        frag = u_default_bg;
    } else {
        frag = texelFetch(u_bg, cell, 0);
    }
}
"#;

const FG_VS: &str = r#"#version 330 core
layout(location = 0) in uvec2 a_cell;
layout(location = 1) in ivec2 a_offset;
layout(location = 2) in uvec2 a_size;
layout(location = 3) in uvec2 a_uv;
layout(location = 4) in vec4 a_color;
layout(location = 5) in uint a_flags;
uniform vec2 u_cell;
uniform vec2 u_pad;
uniform vec2 u_viewport;
out vec2 v_uv;
flat out vec4 v_color;
flat out uint v_flags;
void main() {
    vec2 corner = vec2(gl_VertexID & 1, (gl_VertexID >> 1) & 1);
    vec2 px = u_pad + vec2(a_cell) * u_cell + vec2(a_offset) + corner * vec2(a_size);
    vec2 ndc = px / u_viewport * 2.0 - 1.0;
    gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    v_uv = (vec2(a_uv) + corner * vec2(a_size)) / 2048.0;
    v_color = a_color;
    v_flags = a_flags;
}
"#;

const FG_FS: &str = r#"#version 330 core
uniform sampler2D u_atlas;
in vec2 v_uv;
flat in vec4 v_color;
flat in uint v_flags;
out vec4 frag;
void main() {
    if ((v_flags & 2u) != 0u) {
        frag = vec4(v_color.rgb * v_color.a, v_color.a);
    } else if ((v_flags & 1u) != 0u) {
        frag = texture(u_atlas, v_uv);
    } else {
        float a = texture(u_atlas, v_uv).a * v_color.a;
        frag = vec4(v_color.rgb * a, a);
    }
}
"#;

unsafe fn program(gl: &glow::Context, vs: &str, fs: &str) -> glow::Program {
    unsafe {
        let program = gl.create_program().expect("create program");
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let shader = gl.create_shader(kind).expect("create shader");
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                panic!("shader compile error: {}", gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            panic!("program link error: {}", gl.get_program_info_log(program));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        program
    }
}

unsafe fn uniforms(gl: &glow::Context, program: glow::Program) -> Uniforms {
    unsafe {
        let get = |name| gl.get_uniform_location(program, name).unwrap_or_else(|| panic!("uniform {name}"));
        Uniforms { cell: get("u_cell"), pad: get("u_pad"), viewport: get("u_viewport") }
    }
}

unsafe fn new_texture(gl: &glow::Context, w: i32, h: i32) -> glow::Texture {
    unsafe {
        let texture = gl.create_texture().expect("create texture");
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (param, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
            (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, param, value as i32);
        }
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        texture
    }
}

/// Arms (left, right, up, down) of the light box-drawing characters, drawn
/// as lines so adjacent cells join without gaps.
fn box_arms(ch: u32) -> Option<[bool; 4]> {
    Some(match ch {
        0x2500 => [true, true, false, false],
        0x2502 => [false, false, true, true],
        0x250C => [false, true, false, true],
        0x2510 => [true, false, false, true],
        0x2514 => [false, true, true, false],
        0x2518 => [true, false, true, false],
        0x251C => [false, true, true, true],
        0x2524 => [true, false, true, true],
        0x252C => [true, true, false, true],
        0x2534 => [true, true, true, false],
        0x253C => [true, true, true, true],
        _ => return None,
    })
}

fn bytes_of<T: Copy>(slice: &[T]) -> &[u8] {
    // SAFETY: only used with plain-old-data types without padding (`Instance`, `[u8; 4]`).
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast(), std::mem::size_of_val(slice)) }
}

impl Renderer {
    pub fn new(gl: glow::Context, fonts: Fonts) -> Renderer {
        unsafe {
            let bg_program = program(&gl, BG_VS, BG_FS);
            let bg_uniforms = uniforms(&gl, bg_program);
            let bg_default = gl.get_uniform_location(bg_program, "u_default_bg").expect("u_default_bg");
            let fg_program = program(&gl, FG_VS, FG_FS);
            let fg_uniforms = uniforms(&gl, fg_program);

            let empty_vao = gl.create_vertex_array().expect("vao");
            let fg_vao = gl.create_vertex_array().expect("vao");
            let fg_buffer = gl.create_buffer().expect("buffer");
            gl.bind_vertex_array(Some(fg_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(fg_buffer));
            let stride = size_of::<Instance>() as i32;
            for (index, size, ty, offset) in [
                (0, 2, glow::UNSIGNED_SHORT, 0),
                (1, 2, glow::SHORT, 4),
                (2, 2, glow::UNSIGNED_SHORT, 8),
                (3, 2, glow::UNSIGNED_SHORT, 12),
                (5, 1, glow::UNSIGNED_INT, 20),
            ] {
                gl.vertex_attrib_pointer_i32(index, size, ty, stride, offset);
                gl.vertex_attrib_divisor(index, 1);
                gl.enable_vertex_attrib_array(index);
            }
            gl.vertex_attrib_pointer_f32(4, 4, glow::UNSIGNED_BYTE, true, stride, 16);
            gl.vertex_attrib_divisor(4, 1);
            gl.enable_vertex_attrib_array(4);
            gl.bind_vertex_array(None);

            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            let atlas_texture = new_texture(&gl, ATLAS_SIZE, ATLAS_SIZE);
            let bg_texture = new_texture(&gl, 1, 1);
            gl.use_program(Some(bg_program));
            gl.uniform_1_i32(gl.get_uniform_location(bg_program, "u_bg").as_ref(), 0);
            gl.use_program(Some(fg_program));
            gl.uniform_1_i32(gl.get_uniform_location(fg_program, "u_atlas").as_ref(), 1);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);

            Renderer {
                gl,
                fonts,
                bg_program,
                bg_uniforms,
                bg_default,
                bg_texture,
                bg_size: (1, 1),
                bg_row: Vec::new(),
                fg_program,
                fg_uniforms,
                empty_vao,
                fg_vao,
                fg_buffer,
                fg_capacity: 0,
                atlas: Atlas { texture: atlas_texture, x: 0, y: 0, shelf_height: 0 },
                ascii: Box::new([[None; 128]; 4]),
                glyphs: HashMap::new(),
                rows: Vec::new(),
                upload: Vec::new(),
                viewport: (1, 1),
                padding: (0.0, 0.0),
            }
        }
    }

    pub fn info(&self) -> String {
        unsafe {
            format!(
                "{} / {}",
                self.gl.get_parameter_string(glow::RENDERER),
                self.gl.get_parameter_string(glow::VERSION)
            )
        }
    }

    pub fn cell_size(&self) -> (u32, u32) {
        (self.fonts.cell_width, self.fonts.cell_height)
    }

    /// Sets the drawable size in pixels and the padding before the grid's top-left corner.
    pub fn resize(&mut self, width: u32, height: u32, pad: u32) {
        self.viewport = (width.max(1), height.max(1));
        self.padding = (pad as f32, pad as f32);
        unsafe { self.gl.viewport(0, 0, width as i32, height as i32) };
    }

    fn glyph(&mut self, ch: u32, face: Face) -> Option<Glyph> {
        if ch < 128 {
            if let Some(glyph) = self.ascii[face][ch as usize] {
                return Some(glyph);
            }
        } else if let Some(glyph) = self.glyphs.get(&(ch, face)) {
            return *glyph;
        }
        let glyph = self.load_glyph(ch, face);
        if ch < 128 {
            self.ascii[face][ch as usize] = glyph;
        } else {
            self.glyphs.insert((ch, face), glyph);
        }
        glyph
    }

    fn load_glyph(&mut self, ch: u32, face: Face) -> Option<Glyph> {
        let bitmap = self.fonts.rasterize(char::from_u32(ch)?, face)?;
        if bitmap.width == 0 || bitmap.height == 0 {
            return None;
        }
        let (x, y) = self.atlas.reserve(bitmap.width as i32, bitmap.height as i32)?;
        unsafe {
            self.gl.active_texture(glow::TEXTURE1);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                bitmap.width as i32,
                bitmap.height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&bitmap.rgba)),
            );
        }
        Some(Glyph {
            uv: [x as u16, y as u16],
            size: [bitmap.width as u16, bitmap.height as u16],
            offset: [bitmap.left as i16, (self.fonts.baseline - bitmap.top) as i16],
            colored: bitmap.colored,
        })
    }

    fn rect(cell: [u16; 2], x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) -> Instance {
        Instance {
            cell,
            offset: [x as i16, y as i16],
            size: [w.max(0) as u16, h.max(0) as u16],
            uv: [0, 0],
            color,
            flags: FLAG_RECT,
        }
    }

    fn push_cell(&mut self, out: &mut Vec<Instance>, pos: [u16; 2], cell: &FrameCell, fg: [u8; 4]) {
        let (cw, chh) = (self.fonts.cell_width as i32, self.fonts.cell_height as i32);
        if let Some([left, right, up, down]) = box_arms(cell.ch) {
            let t = (chh / 16).max(1);
            let (mx, my) = ((cw - t) / 2, (chh - t) / 2);
            if left {
                out.push(Self::rect(pos, 0, my, mx + t, t, fg));
            }
            if right {
                out.push(Self::rect(pos, mx, my, cw - mx, t, fg));
            }
            if up {
                out.push(Self::rect(pos, mx, 0, t, my + t, fg));
            }
            if down {
                out.push(Self::rect(pos, mx, my, t, chh - my, fg));
            }
        } else if cell.ch > b' ' as u32 {
            let face = face_for(cell.flags & BOLD != 0, cell.flags & ITALIC != 0);
            if let Some(g) = self.glyph(cell.ch, face) {
                out.push(Instance {
                    cell: pos,
                    offset: g.offset,
                    size: g.size,
                    uv: g.uv,
                    color: fg,
                    flags: if g.colored { FLAG_COLORED } else { 0 },
                });
            }
        }
        let width = if cell.flags & WIDE != 0 { 2 * cw } else { cw };
        if cell.flags & UNDERLINE != 0 {
            out.push(Self::rect(pos, 0, self.fonts.underline_y, width, self.fonts.underline_thickness, fg));
        }
        if cell.flags & STRIKE != 0 {
            out.push(Self::rect(pos, 0, self.fonts.strike_y, width, self.fonts.underline_thickness, fg));
        }
    }

    fn rebuild_row(&mut self, frame: &Frame, y: u16) {
        let mut out = std::mem::take(&mut self.rows[y as usize]);
        out.clear();
        for (x, cell) in frame.row(y).iter().enumerate() {
            if cell.ch != 0 || cell.flags & (UNDERLINE | STRIKE) != 0 {
                self.push_cell(&mut out, [x as u16, y], cell, cell.fg);
            }
        }
        self.rows[y as usize] = out;
    }

    /// Draws the frame and clears its dirty marks.
    pub fn draw(&mut self, frame: &mut Frame) {
        let (cols, rows) = (frame.cols(), frame.rows());
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.bg_texture));
            if self.bg_size != (cols, rows) {
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA8 as i32,
                    cols as i32,
                    rows as i32,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(None),
                );
                self.bg_size = (cols, rows);
                self.rows = vec![Vec::new(); rows as usize];
                for y in 0..rows {
                    frame.row_mut(y);
                }
            }
        }
        // All background rows first: loading a glyph switches the active texture unit.
        for y in (0..rows).filter(|&y| frame.is_dirty(y)) {
            self.bg_row.clear();
            self.bg_row.extend(frame.row(y).iter().map(|c| c.bg));
            unsafe {
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    y as i32,
                    cols as i32,
                    1,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(bytes_of(&self.bg_row))),
                );
            }
        }
        for y in (0..rows).filter(|&y| frame.is_dirty(y)) {
            self.rebuild_row(frame, y);
        }
        frame.clear_dirty();

        let mut upload = std::mem::take(&mut self.upload);
        upload.clear();
        for row in &self.rows {
            upload.extend_from_slice(row);
        }
        if let Some(cursor) = frame.cursor.filter(|c| c.x < cols && c.y < rows) {
            let (cw, ch) = (self.fonts.cell_width as i32, self.fonts.cell_height as i32);
            let pos = [cursor.x, cursor.y];
            let width = if cursor.wide { 2 * cw } else { cw };
            match cursor.shape {
                term::CursorShape::Block => {
                    upload.push(Self::rect(pos, 0, 0, width, ch, cursor.color));
                    let cell = frame.row(cursor.y)[cursor.x as usize];
                    self.push_cell(&mut upload, pos, &cell, cursor.text_color);
                }
                term::CursorShape::Underline => upload.push(Self::rect(pos, 0, ch - 2, width, 2, cursor.color)),
                term::CursorShape::Bar => upload.push(Self::rect(pos, 0, 0, 2, ch, cursor.color)),
            }
        }

        let gl = &self.gl;
        let (vw, vh) = (self.viewport.0 as f32, self.viewport.1 as f32);
        let (cw, ch) = (self.fonts.cell_width as f32, self.fonts.cell_height as f32);
        unsafe {
            let bg = frame.default_bg.map(|c| c as f32 / 255.0);
            gl.use_program(Some(self.bg_program));
            gl.uniform_2_f32(Some(&self.bg_uniforms.cell), cw, ch);
            gl.uniform_2_f32(Some(&self.bg_uniforms.pad), self.padding.0, self.padding.1);
            gl.uniform_2_f32(Some(&self.bg_uniforms.viewport), vw, vh);
            gl.uniform_4_f32(Some(&self.bg_default), bg[0], bg[1], bg[2], bg[3]);
            gl.bind_vertex_array(Some(self.empty_vao));
            gl.disable(glow::BLEND);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            if !upload.is_empty() {
                gl.enable(glow::BLEND);
                gl.use_program(Some(self.fg_program));
                gl.uniform_2_f32(Some(&self.fg_uniforms.cell), cw, ch);
                gl.uniform_2_f32(Some(&self.fg_uniforms.pad), self.padding.0, self.padding.1);
                gl.uniform_2_f32(Some(&self.fg_uniforms.viewport), vw, vh);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
                gl.bind_vertex_array(Some(self.fg_vao));
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.fg_buffer));
                if upload.len() > self.fg_capacity {
                    self.fg_capacity = upload.len().next_power_of_two();
                    gl.buffer_data_size(
                        glow::ARRAY_BUFFER,
                        (self.fg_capacity * size_of::<Instance>()) as i32,
                        glow::STREAM_DRAW,
                    );
                }
                gl.buffer_sub_data_u8_slice(glow::ARRAY_BUFFER, 0, bytes_of(&upload));
                gl.draw_arrays_instanced(glow::TRIANGLE_STRIP, 0, 4, upload.len() as i32);
            }
            if cfg!(debug_assertions) {
                let error = gl.get_error();
                assert_eq!(error, glow::NO_ERROR, "GL error 0x{error:x}");
            }
        }
        self.upload = upload;
    }
}
