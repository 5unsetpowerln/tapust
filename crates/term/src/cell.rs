//! Compact cell and style representation.
//!
//! A cell is a single `u64` so that bulk printing writes one word per column
//! and rendering reads one word per column. Colors and attributes live in a
//! per-terminal style table referenced by a 16-bit id; id 0 is the default style.

/// Width class of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Width {
    /// A single-column character, or an empty cell.
    Narrow = 0,
    /// The first column of a double-width character.
    Wide = 1,
    /// The second column of a double-width character; holds no codepoint.
    SpacerTail = 2,
    /// Placeholder in the last column when a wide character wrapped to the next row.
    SpacerHead = 3,
}

/// One grid cell packed into 64 bits.
///
/// - bits 0..21: Unicode scalar value (0 means empty)
/// - bits 21..23: [`Width`]
/// - bit 23: the cell has extra (combining) codepoints in its row's grapheme table
/// - bits 24..26: [`Semantic`] content (OSC 133) of a printed cell
/// - bits 26..32: reserved
/// - bits 32..48: style id (0 = default style)
/// - bits 48..64: reserved
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Cell(pub u64);

impl Cell {
    pub const EMPTY: Cell = Cell(0);

    const CP_MASK: u64 = (1 << 21) - 1;
    const WIDTH_SHIFT: u32 = 21;
    const GRAPHEME_BIT: u64 = 1 << 23;
    const STYLE_SHIFT: u32 = 32;
    pub(crate) const SEMANTIC_SHIFT: u32 = 24;
    pub(crate) const SEMANTIC_MASK: u64 = 3 << Self::SEMANTIC_SHIFT;

    #[inline(always)]
    pub const fn new(cp: u32, width: Width, style: StyleId) -> Cell {
        Cell((cp as u64 & Self::CP_MASK) | ((width as u64) << Self::WIDTH_SHIFT) | ((style.0 as u64) << Self::STYLE_SHIFT))
    }

    /// The codepoint, or 0 for an empty cell.
    #[inline(always)]
    pub const fn codepoint(self) -> u32 {
        (self.0 & Self::CP_MASK) as u32
    }

    #[inline(always)]
    /// What the shell said the cell is (OSC 133): output unless printed
    /// while a prompt or the input was being written.
    pub fn semantic(self) -> Semantic {
        match (self.0 & Self::SEMANTIC_MASK) >> Self::SEMANTIC_SHIFT {
            1 => Semantic::Prompt,
            2 => Semantic::Input,
            _ => Semantic::Output,
        }
    }

    pub fn char(self) -> Option<char> {
        match self.codepoint() {
            0 => None,
            cp => char::from_u32(cp),
        }
    }

    #[inline(always)]
    pub const fn width(self) -> Width {
        match (self.0 >> Self::WIDTH_SHIFT) & 3 {
            0 => Width::Narrow,
            1 => Width::Wide,
            2 => Width::SpacerTail,
            _ => Width::SpacerHead,
        }
    }

    #[inline(always)]
    pub const fn has_grapheme(self) -> bool {
        self.0 & Self::GRAPHEME_BIT != 0
    }

    #[inline(always)]
    pub const fn with_grapheme(self, on: bool) -> Cell {
        if on { Cell(self.0 | Self::GRAPHEME_BIT) } else { Cell(self.0 & !Self::GRAPHEME_BIT) }
    }

    #[inline(always)]
    pub const fn style(self) -> StyleId {
        StyleId((self.0 >> Self::STYLE_SHIFT) as u16)
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cell")
            .field("char", &self.char())
            .field("width", &self.width())
            .field("grapheme", &self.has_grapheme())
            .field("style", &self.style().0)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StyleId(pub u16);

impl StyleId {
    pub const DEFAULT: StyleId = StyleId(0);
}

/// A color as written by the application; the renderer resolves it with the
/// terminal's [`crate::Palette`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The default foreground or background, depending on where it is used.
    #[default]
    Default,
    /// One of the 256 palette entries.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Underline shape (SGR 4, 4:n, 21).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Underline {
    #[default]
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// Text attribute flags (SGR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs(pub u16);

impl Attrs {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const BLINK: u16 = 1 << 3;
    pub const INVERSE: u16 = 1 << 4;
    pub const HIDDEN: u16 = 1 << 5;
    pub const STRIKE: u16 = 1 << 6;
    pub const OVERLINE: u16 = 1 << 7;

    #[inline(always)]
    pub const fn contains(self, flag: u16) -> bool {
        self.0 & flag != 0
    }
}

/// Everything about a cell's appearance except its content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// Underline color (SGR 58); `Default` means "same as foreground".
    pub underline_color: Color,
    pub underline: Underline,
    pub attrs: Attrs,
}

impl Style {
    pub const DEFAULT: Style = Style {
        fg: Color::Default,
        bg: Color::Default,
        underline_color: Color::Default,
        underline: Underline::None,
        attrs: Attrs(0),
    };
}

/// ghostty's semantic content of a cell, from OSC 133 shell integration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Semantic {
    #[default]
    Output = 0,
    Prompt = 1,
    Input = 2,
}
