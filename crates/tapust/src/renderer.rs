//! OpenGL 3.3 renderer.
//!
//! After clearing to the window background:
//! 1. Backgrounds: one quad per pane whose fragment shader looks up the cell
//!    under each pixel in the pane's `cols x rows` RGBA texture, and shows
//!    the pane's background around the cells.
//! 2. Foreground: one instanced draw of quads in window pixels for glyphs
//!    and underlines (from one RGBA atlas) and rectangles (cursors,
//!    strikethroughs, box-drawing lines).
//!
//! Only dirty grid rows rebuild their background texels and instances.

use std::collections::HashMap;

use glow::HasContext;
use term::Underline;

use crate::font::{Face, Fonts, face_for};
use crate::frame::{BOLD, CursorStyle, Frame, FrameCell, Grid, ITALIC, STRIKE, TRANSLUCENT, WIDE};
use crate::underline;

const ATLAS_SIZE: i32 = 2048;
const FLAG_COLORED: u32 = 1;
const FLAG_RECT: u32 = 2;

/// A quad whose top-left corner is at `pos` in window pixels.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
struct Instance {
    pos: [i16; 2],
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

/// What the renderer keeps for one pane grid between frames.
struct GridCache {
    texture: glow::Texture,
    size: (u16, u16),
    origin: (i32, i32),
    rows: Vec<Vec<Instance>>,
}

struct BgUniforms {
    rect: glow::UniformLocation,
    origin: glow::UniformLocation,
    cell: glow::UniformLocation,
    viewport: glow::UniformLocation,
    pad_bg: glow::UniformLocation,
    extend: glow::UniformLocation,
}

pub struct Renderer {
    gl: glow::Context,
    pub fonts: Fonts,
    bg_program: glow::Program,
    bg_uniforms: BgUniforms,
    bg_row: Vec<[u8; 4]>,
    grids: HashMap<u32, GridCache>,
    fg_program: glow::Program,
    fg_viewport: glow::UniformLocation,
    empty_vao: glow::VertexArray,
    fg_vao: glow::VertexArray,
    fg_buffer: glow::Buffer,
    fg_capacity: usize,
    atlas: Atlas,
    ascii: Box<[[Option<Glyph>; 128]; 4]>,
    glyphs: HashMap<(u32, Face), Option<Glyph>>,
    /// The instance of each underline kind (by `Underline as usize`) for a
    /// cell at (0, 0) in color 0, measured from `fonts`; empty until the
    /// first frame drawn with these fonts.
    underlines: Vec<Instance>,
    upload: Vec<Instance>,
    viewport: (u32, u32),
}

const BG_VS: &str = r#"#version 330 core
uniform vec4 u_rect;
uniform vec2 u_viewport;
void main() {
    vec2 corner = vec2(gl_VertexID & 1, (gl_VertexID >> 1) & 1);
    vec2 ndc = (u_rect.xy + corner * u_rect.zw) / u_viewport * 2.0 - 1.0;
    gl_Position = vec4(ndc.x, -ndc.y, 0.0, 1.0);
}
"#;

const BG_FS: &str = r#"#version 330 core
uniform sampler2D u_bg;
uniform vec2 u_origin;
uniform vec2 u_cell;
uniform vec2 u_viewport;
uniform vec4 u_pad_bg;
// The sides whose padding takes the nearest cell's background: 1 left,
// 2 right, 4 up, 8 down (ghostty's cell_bg).
uniform int u_extend;
out vec4 frag;
void main() {
    vec2 px = vec2(gl_FragCoord.x, u_viewport.y - gl_FragCoord.y);
    ivec2 cell = ivec2(floor((px - u_origin) / u_cell));
    ivec2 size = textureSize(u_bg, 0);
    bool pad = false;
    if (cell.x < 0) {
        if ((u_extend & 1) != 0) cell.x = 0; else pad = true;
    } else if (cell.x >= size.x) {
        if ((u_extend & 2) != 0) cell.x = size.x - 1; else pad = true;
    }
    if (cell.y < 0) {
        if ((u_extend & 4) != 0) cell.y = 0; else pad = true;
    } else if (cell.y >= size.y) {
        if ((u_extend & 8) != 0) cell.y = size.y - 1; else pad = true;
    }
    frag = pad ? u_pad_bg : texelFetch(u_bg, cell, 0);
}
"#;

const FG_VS: &str = r#"#version 330 core
layout(location = 0) in ivec2 a_pos;
layout(location = 1) in uvec2 a_size;
layout(location = 2) in uvec2 a_uv;
layout(location = 3) in vec4 a_color;
layout(location = 4) in uint a_flags;
uniform vec2 u_viewport;
out vec2 v_uv;
flat out vec4 v_color;
flat out uint v_flags;
void main() {
    vec2 corner = vec2(gl_VertexID & 1, (gl_VertexID >> 1) & 1);
    vec2 px = vec2(a_pos) + corner * vec2(a_size);
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

unsafe fn uniform(gl: &glow::Context, program: glow::Program, name: &str) -> glow::UniformLocation {
    unsafe { gl.get_uniform_location(program, name).unwrap_or_else(|| panic!("uniform {name}")) }
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

/// An opaque color at `alpha`, premultiplied as GTK takes the GLArea's
/// pixels.
fn premultiply(c: [u8; 4], alpha: u8) -> [u8; 4] {
    let mul = |v: u8| ((u16::from(v) * u16::from(alpha) + 127) / 255) as u8;
    [mul(c[0]), mul(c[1]), mul(c[2]), alpha]
}

fn bytes_of<T: Copy>(slice: &[T]) -> &[u8] {
    // SAFETY: only used with plain-old-data types without padding (`Instance`, `[u8; 4]`).
    unsafe { std::slice::from_raw_parts(slice.as_ptr().cast(), std::mem::size_of_val(slice)) }
}

impl Renderer {
    pub fn new(gl: glow::Context, fonts: Fonts) -> Renderer {
        unsafe {
            let bg_program = program(&gl, BG_VS, BG_FS);
            let bg_uniforms = BgUniforms {
                rect: uniform(&gl, bg_program, "u_rect"),
                origin: uniform(&gl, bg_program, "u_origin"),
                cell: uniform(&gl, bg_program, "u_cell"),
                viewport: uniform(&gl, bg_program, "u_viewport"),
                pad_bg: uniform(&gl, bg_program, "u_pad_bg"),
                extend: uniform(&gl, bg_program, "u_extend"),
            };
            let fg_program = program(&gl, FG_VS, FG_FS);
            let fg_viewport = uniform(&gl, fg_program, "u_viewport");

            let empty_vao = gl.create_vertex_array().expect("vao");
            let fg_vao = gl.create_vertex_array().expect("vao");
            let fg_buffer = gl.create_buffer().expect("buffer");
            gl.bind_vertex_array(Some(fg_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(fg_buffer));
            let stride = size_of::<Instance>() as i32;
            for (index, size, ty, offset) in [
                (0, 2, glow::SHORT, 0),
                (1, 2, glow::UNSIGNED_SHORT, 4),
                (2, 2, glow::UNSIGNED_SHORT, 8),
                (4, 1, glow::UNSIGNED_INT, 16),
            ] {
                gl.vertex_attrib_pointer_i32(index, size, ty, stride, offset);
                gl.vertex_attrib_divisor(index, 1);
                gl.enable_vertex_attrib_array(index);
            }
            gl.vertex_attrib_pointer_f32(3, 4, glow::UNSIGNED_BYTE, true, stride, 12);
            gl.vertex_attrib_divisor(3, 1);
            gl.enable_vertex_attrib_array(3);
            gl.bind_vertex_array(None);

            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            let atlas_texture = new_texture(&gl, ATLAS_SIZE, ATLAS_SIZE);
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
                bg_row: Vec::new(),
                grids: HashMap::new(),
                fg_program,
                fg_viewport,
                empty_vao,
                fg_vao,
                fg_buffer,
                fg_capacity: 0,
                atlas: Atlas { texture: atlas_texture, x: 0, y: 0, shelf_height: 0 },
                ascii: Box::new([[None; 128]; 4]),
                glyphs: HashMap::new(),
                underlines: Vec::new(),
                upload: Vec::new(),
                viewport: (1, 1),
            }
        }
    }

    /// Switches to fonts of another size or scale: every cached glyph and
    /// grid is drawn again. Needs the GL context current.
    pub fn set_fonts(&mut self, fonts: Fonts) {
        self.fonts = fonts;
        *self.ascii = [[None; 128]; 4];
        self.glyphs.clear();
        self.underlines.clear();
        for (_, cache) in self.grids.drain() {
            unsafe { self.gl.delete_texture(cache.texture) };
        }
        self.atlas.x = 0;
        self.atlas.y = 0;
        self.atlas.shelf_height = 0;
    }

    pub fn cell_size(&self) -> (u32, u32) {
        (self.fonts.cell_width, self.fonts.cell_height)
    }

    /// Sets the drawable size in pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.viewport = (width.max(1), height.max(1));
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
            // Blank glyphs are cached as empty, not as None, so they are not rasterized again.
            return Some(Glyph { uv: [0, 0], size: [0, 0], offset: [0, 0], colored: false });
        }
        Some(Glyph {
            uv: self.add_to_atlas(bitmap.width, bitmap.height, &bitmap.rgba)?,
            size: [bitmap.width as u16, bitmap.height as u16],
            offset: [bitmap.left as i16, (self.fonts.baseline - bitmap.top) as i16],
            colored: bitmap.colored,
        })
    }

    /// Copies a `width x height` RGBA bitmap into the atlas; returns its
    /// position there, or None when the atlas is full.
    fn add_to_atlas(&mut self, width: u32, height: u32, rgba: &[u8]) -> Option<[u16; 2]> {
        let (x, y) = self.atlas.reserve(width as i32, height as i32)?;
        unsafe {
            self.gl.active_texture(glow::TEXTURE1);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas.texture));
            self.gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                width as i32,
                height as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
        }
        Some([x as u16, y as u16])
    }

    /// Draws the underline kinds for the current fonts into the atlas, as
    /// coverage like glyphs.
    fn build_underlines(&mut self) -> Vec<Instance> {
        let metrics = underline::Metrics {
            cell_width: self.fonts.cell_width,
            cell_height: self.fonts.cell_height,
            position: self.fonts.underline_y,
            thickness: self.fonts.underline_thickness as u32,
        };
        let mut underlines = vec![Instance::default(); Underline::Dashed as usize + 1];
        for kind in [Underline::Single, Underline::Double, Underline::Curly, Underline::Dotted, Underline::Dashed] {
            let s = underline::sprite(kind, metrics);
            let rgba: Vec<u8> = s.alpha.iter().flat_map(|&a| [255, 255, 255, a]).collect();
            // The atlas was emptied with the fonts the sprites are measured from.
            let uv = self.add_to_atlas(s.width, s.height, &rgba).expect("atlas space for the underlines");
            let (pos, size) = ([s.left as i16, s.top as i16], [s.width as u16, s.height as u16]);
            underlines[kind as usize] = Instance { pos, size, uv, color: [0; 4], flags: 0 };
        }
        underlines
    }

    fn rect(x: i32, y: i32, w: i32, h: i32, color: [u8; 4]) -> Instance {
        Instance {
            pos: [x as i16, y as i16],
            size: [w.max(0) as u16, h.max(0) as u16],
            uv: [0, 0],
            color,
            flags: FLAG_RECT,
        }
    }

    /// The instances of `cell` with its top-left corner at (`px`, `py`), in
    /// the cell's colors or all in `over`.
    fn push_cell(&mut self, out: &mut Vec<Instance>, px: i32, py: i32, cell: &FrameCell, over: Option<[u8; 4]>) {
        let (cw, chh) = (self.fonts.cell_width as i32, self.fonts.cell_height as i32);
        let fg = over.unwrap_or(cell.fg);
        // ghostty draws underlines under the glyphs, so that a colored one
        // does not cover the descenders.
        if cell.underline != Underline::None {
            let sprite = self.underlines[cell.underline as usize];
            let pos = [(px + sprite.pos[0] as i32) as i16, (py + sprite.pos[1] as i32) as i16];
            out.push(Instance { pos, color: over.or(cell.underline_color).unwrap_or(cell.fg), ..sprite });
        }
        if let Some([left, right, up, down]) = box_arms(cell.ch) {
            let t = (chh / 16).max(1);
            let (mx, my) = ((cw - t) / 2, (chh - t) / 2);
            if left {
                out.push(Self::rect(px, py + my, mx + t, t, fg));
            }
            if right {
                out.push(Self::rect(px + mx, py + my, cw - mx, t, fg));
            }
            if up {
                out.push(Self::rect(px + mx, py, t, my + t, fg));
            }
            if down {
                out.push(Self::rect(px + mx, py + my, t, chh - my, fg));
            }
        } else if cell.ch > b' ' as u32 {
            let face = face_for(cell.flags & BOLD != 0, cell.flags & ITALIC != 0);
            if let Some(g) = self.glyph(cell.ch, face).filter(|g| g.size[0] > 0) {
                out.push(Instance {
                    pos: [(px + g.offset[0] as i32) as i16, (py + g.offset[1] as i32) as i16],
                    size: g.size,
                    uv: g.uv,
                    color: fg,
                    flags: if g.colored { FLAG_COLORED } else { 0 },
                });
            }
        }
        if cell.flags & STRIKE != 0 {
            let width = if cell.flags & WIDE != 0 { 2 * cw } else { cw };
            out.push(Self::rect(px, py + self.fonts.strike_y, width, self.fonts.underline_thickness, fg));
        }
    }

    fn rebuild_row(&mut self, cache: &mut GridCache, grid: &Grid, y: u16) {
        let (cw, ch) = (self.fonts.cell_width as i32, self.fonts.cell_height as i32);
        let mut out = std::mem::take(&mut cache.rows[y as usize]);
        out.clear();
        let py = grid.y + y as i32 * ch;
        for (x, cell) in grid.row(y).iter().enumerate() {
            if cell.ch != 0 || cell.underline != Underline::None || cell.flags & STRIKE != 0 {
                self.push_cell(&mut out, grid.x + x as i32 * cw, py, cell, None);
            }
        }
        cache.rows[y as usize] = out;
    }

    /// Uploads the dirty background rows of `grid`, (re)creating its cache;
    /// translucent cells take `opacity`.
    fn upload_background(&mut self, grid: &mut Grid, opacity: u8) {
        let (cols, rows) = (grid.cols(), grid.rows());
        let gl = &self.gl;
        let cache = self.grids.entry(grid.id).or_insert_with(|| GridCache {
            texture: unsafe { new_texture(gl, 1, 1) },
            size: (0, 0),
            origin: (grid.x, grid.y),
            rows: Vec::new(),
        });
        unsafe {
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(cache.texture));
            if cache.size != (cols, rows) {
                gl.tex_image_2d(
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
                cache.size = (cols, rows);
                cache.rows = vec![Vec::new(); rows as usize];
                grid.mark_dirty();
            }
        }
        if cache.origin != (grid.x, grid.y) {
            // Instances are in window pixels: a moved pane rebuilds them all.
            cache.origin = (grid.x, grid.y);
            grid.mark_dirty();
        }
        for y in (0..rows).filter(|&y| grid.is_dirty(y)) {
            self.bg_row.clear();
            let texel = |c: &FrameCell| if c.flags & TRANSLUCENT != 0 { premultiply(c.bg, opacity) } else { c.bg };
            self.bg_row.extend(grid.row(y).iter().map(texel));
            unsafe {
                gl.tex_sub_image_2d(
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
    }

    /// Draws the frame and clears its grids' dirty marks.
    pub fn draw(&mut self, frame: &mut Frame) {
        let gl = &self.gl;
        self.grids.retain(|id, cache| {
            let keep = frame.grids.iter().any(|g| g.id == *id);
            if !keep {
                unsafe { gl.delete_texture(cache.texture) };
            }
            keep
        });
        // All background rows first: loading a glyph switches the active texture unit.
        for grid in &mut frame.grids {
            self.upload_background(grid, frame.opacity);
        }
        if self.underlines.is_empty() {
            self.underlines = self.build_underlines();
        }
        let mut upload = std::mem::take(&mut self.upload);
        upload.clear();
        for grid in &mut frame.grids {
            let mut cache = self.grids.remove(&grid.id).expect("grid cache");
            for y in (0..grid.rows()).filter(|&y| grid.is_dirty(y)) {
                self.rebuild_row(&mut cache, grid, y);
            }
            grid.clear_dirty();
            for row in &cache.rows {
                upload.extend_from_slice(row);
            }
            self.grids.insert(grid.id, cache);
        }
        let (cw, ch) = (self.fonts.cell_width as i32, self.fonts.cell_height as i32);
        for cursor in &frame.cursors {
            let Some(grid) = frame.grids.get(cursor.grid).filter(|g| cursor.x < g.cols() && cursor.y < g.rows()) else {
                continue;
            };
            let (x, y) = (grid.x + cursor.x as i32 * cw, grid.y + cursor.y as i32 * ch);
            let width = if cursor.wide { 2 * cw } else { cw };
            match cursor.style {
                CursorStyle::Block => {
                    upload.push(Self::rect(x, y, width, ch, cursor.color));
                    // Both cells of a wide character, as each draws its own underline.
                    let cells = grid.row(cursor.y)[cursor.x as usize..].iter().take(1 + cursor.wide as usize);
                    for (i, cell) in cells.enumerate() {
                        self.push_cell(&mut upload, x + i as i32 * cw, y, cell, Some(cursor.text_color));
                    }
                }
                CursorStyle::Underline => upload.push(Self::rect(x, y + ch - 2, width, 2, cursor.color)),
                CursorStyle::Bar => upload.push(Self::rect(x, y, 2, ch, cursor.color)),
                CursorStyle::Lock => {
                    // ghostty renders U+F023 (the Nerd Font lock) in the
                    // cursor color in place of a cursor shape.
                    let lock = FrameCell { ch: 0xF023, fg: cursor.color, ..FrameCell::blank([0; 4]) };
                    self.push_cell(&mut upload, x, y, &lock, None);
                }
                CursorStyle::Hollow => {
                    // A 1px outline, like ghostty's block_hollow with cursor_thickness 1.
                    upload.push(Self::rect(x, y, width, 1, cursor.color));
                    upload.push(Self::rect(x, y + ch - 1, width, 1, cursor.color));
                    upload.push(Self::rect(x, y, 1, ch, cursor.color));
                    upload.push(Self::rect(x + width - 1, y, 1, ch, cursor.color));
                }
            }
        }

        let gl = &self.gl;
        let (vw, vh) = (self.viewport.0 as f32, self.viewport.1 as f32);
        unsafe {
            let bg = premultiply(frame.window_bg, frame.opacity).map(|c| c as f32 / 255.0);
            gl.disable(glow::BLEND);
            gl.clear_color(bg[0], bg[1], bg[2], bg[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(self.bg_program));
            gl.uniform_2_f32(Some(&self.bg_uniforms.cell), cw as f32, ch as f32);
            gl.uniform_2_f32(Some(&self.bg_uniforms.viewport), vw, vh);
            gl.bind_vertex_array(Some(self.empty_vao));
            gl.active_texture(glow::TEXTURE0);
            for grid in &frame.grids {
                let cache = &self.grids[&grid.id];
                gl.bind_texture(glow::TEXTURE_2D, Some(cache.texture));
                gl.uniform_2_f32(Some(&self.bg_uniforms.origin), grid.x as f32, grid.y as f32);
                let (x, y, w, h) = grid.pane;
                gl.uniform_4_f32(Some(&self.bg_uniforms.rect), x as f32, y as f32, w as f32, h as f32);
                let pad = premultiply(grid.bg, frame.opacity).map(|c| c as f32 / 255.0);
                gl.uniform_4_f32(Some(&self.bg_uniforms.pad_bg), pad[0], pad[1], pad[2], pad[3]);
                let e = grid.extend;
                let extend = e.left as i32 | (e.right as i32) << 1 | (e.up as i32) << 2 | (e.down as i32) << 3;
                gl.uniform_1_i32(Some(&self.bg_uniforms.extend), extend);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            if !upload.is_empty() {
                gl.enable(glow::BLEND);
                gl.use_program(Some(self.fg_program));
                gl.uniform_2_f32(Some(&self.fg_viewport), vw, vh);
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

#[cfg(test)]
mod tests {
    use super::premultiply;

    #[test]
    fn colors_are_premultiplied_by_the_opacity() {
        assert_eq!(premultiply([40, 44, 52, 255], 255), [40, 44, 52, 255]);
        assert_eq!(premultiply([40, 44, 52, 255], 204), [32, 35, 42, 204]);
        assert_eq!(premultiply([255, 0, 128, 255], 0), [0, 0, 0, 0]);
    }
}
