//! Font loading and rasterization (FreeType and fontconfig through crossfont).

use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

use crossfont::{
    BitmapBuffer, FontDesc, FontKey, GlyphKey, Rasterize, RasterizedGlyph, Rasterizer, Size, Slant,
    Style, Weight,
};

/// ghostty's default fonts, bundled: JetBrains Mono 2.304 (OFL) and Symbols
/// Nerd Font 3.4.0 (MIT).
const EMBEDDED: [(&str, &[u8]); 5] = [
    ("JetBrainsMono-Regular", include_bytes!("../fonts/JetBrainsMono-Regular.ttf")),
    ("JetBrainsMono-Bold", include_bytes!("../fonts/JetBrainsMono-Bold.ttf")),
    ("JetBrainsMono-Italic", include_bytes!("../fonts/JetBrainsMono-Italic.ttf")),
    ("JetBrainsMono-BoldItalic", include_bytes!("../fonts/JetBrainsMono-BoldItalic.ttf")),
    ("SymbolsNerdFont-Regular", include_bytes!("../fonts/SymbolsNerdFont-Regular.ttf")),
];

#[link(name = "fontconfig")]
unsafe extern "C" {
    fn FcConfigAppFontAddFile(config: *mut libc::c_void, file: *const libc::c_uchar) -> libc::c_int;
}

/// Makes the bundled fonts visible to fontconfig, and so to crossfont,
/// without writing them to disk: each font lives in a memfd that stays open
/// for the life of the process and is added as an application font through
/// its `/proc/self/fd` path.
pub fn register_embedded_fonts() -> io::Result<()> {
    for (name, data) in EMBEDDED {
        let name = CString::new(name).expect("font name");
        let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        file.write_all(data)?;
        let path = CString::new(format!("/proc/self/fd/{fd}")).expect("path");
        if unsafe { FcConfigAppFontAddFile(std::ptr::null_mut(), path.as_ptr().cast()) } == 0 {
            return Err(io::Error::other(format!("fontconfig rejected the bundled font {name:?}")));
        }
        // fontconfig and FreeType open the font by this path later; keep it open.
        let _ = file.into_raw_fd();
    }
    Ok(())
}

/// Index of one of the four faces: regular, bold, italic, bold italic.
pub type Face = usize;

pub fn face_for(bold: bool, italic: bool) -> Face {
    (bold as usize) | ((italic as usize) << 1)
}

/// A rasterized glyph in RGBA, ready for the atlas.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Pixels from the cell origin to the bitmap's left edge.
    pub left: i32,
    /// Pixels from the baseline up to the bitmap's top edge.
    pub top: i32,
    /// Color glyphs (emoji) carry premultiplied RGBA; others carry coverage in alpha.
    pub colored: bool,
    pub rgba: Vec<u8>,
}

pub struct Fonts {
    rasterizer: Rasterizer,
    faces: [FontKey; 4],
    size: Size,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Distance from the cell top to the baseline.
    pub baseline: i32,
    /// Underline top, relative to the cell top.
    pub underline_y: i32,
    pub underline_thickness: i32,
    pub strike_y: i32,
}

impl Fonts {
    pub fn new(family: &str, points: f32, scale: f64) -> Result<Fonts, crossfont::Error> {
        let mut rasterizer = Rasterizer::new()?;
        let size = Size::new(points * scale as f32);
        let desc = |slant, weight| FontDesc::new(family, Style::Description { slant, weight });
        let regular = rasterizer.load_font(&desc(Slant::Normal, Weight::Normal), size)?;
        // A missing bold or italic face uses the regular one, like other terminals.
        let mut load = |slant, weight| rasterizer.load_font(&desc(slant, weight), size).unwrap_or(regular);
        let bold = load(Slant::Normal, Weight::Bold);
        let italic = load(Slant::Italic, Weight::Normal);
        let bold_italic = load(Slant::Italic, Weight::Bold);

        // FreeType needs one glyph loaded before the face's metrics are available.
        rasterizer.get_glyph(GlyphKey { font_key: regular, character: 'm', size })?;
        let metrics = rasterizer.metrics(regular, size)?;
        let cell_width = metrics.average_advance.ceil().max(1.0) as u32;
        let cell_height = metrics.line_height.ceil().max(1.0) as u32;
        let baseline = (cell_height as f32 + metrics.descent).round() as i32;
        let underline_thickness = metrics.underline_thickness.round().max(1.0) as i32;
        let underline_y = baseline - metrics.underline_position.round() as i32 - underline_thickness / 2;
        let strike_y = baseline - metrics.strikeout_position.round() as i32;
        Ok(Fonts {
            rasterizer,
            faces: [regular, bold, italic, bold_italic],
            size,
            cell_width,
            cell_height,
            baseline,
            underline_y: underline_y.min(cell_height as i32 - underline_thickness),
            underline_thickness,
            strike_y,
        })
    }

    /// Rasterizes `ch` in `face`; a character missing from every font yields
    /// the replacement glyph.
    pub fn rasterize(&mut self, ch: char, face: Face) -> Option<Bitmap> {
        let key = GlyphKey { font_key: self.faces[face], character: ch, size: self.size };
        let glyph = match self.rasterizer.get_glyph(key) {
            Ok(glyph) | Err(crossfont::Error::MissingGlyph(glyph)) => glyph,
            Err(_) => return None,
        };
        Some(to_bitmap(glyph))
    }
}

fn to_bitmap(glyph: RasterizedGlyph) -> Bitmap {
    let (width, height) = (glyph.width.max(0) as u32, glyph.height.max(0) as u32);
    let (colored, rgba) = match glyph.buffer {
        // Grayscale or subpixel coverage: keep one coverage value in alpha.
        BitmapBuffer::Rgb(rgb) => (
            false,
            rgb.chunks_exact(3)
                .flat_map(|p| [255, 255, 255, ((p[0] as u16 + p[1] as u16 + p[2] as u16) / 3) as u8])
                .collect(),
        ),
        BitmapBuffer::Rgba(rgba) => (true, rgba),
    };
    Bitmap { width, height, left: glyph.left, top: glyph.top, colored, rgba }
}
