//! Character attributes (SGR): colours and text styles per screen cell.
//!
//! Full-screen programs mark the selected menu item, list row or dialog
//! control by drawing it in a different colour, not by moving the cursor.
//! Keeping the attributes of every cell is what lets the TUI tracker
//! (`crate::tui`) find and announce that highlight.

/// A colour as set by SGR. Only equality matters to the screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The terminal's default foreground or background
    #[default]
    Default,
    /// One of the 256 indexed colours (0-7 normal, 8-15 bright)
    Indexed(u8),
    /// A 24-bit colour
    Rgb(u8, u8, u8),
}

/// Rendition of a cell: colours plus the style flags that affect how a
/// highlight is recognised. Italic, strikethrough and blink are parsed but
/// not stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Attrs {
    /// The colour actually painted behind the cell: the background, or the
    /// foreground when reverse video is on (a reversed default foreground
    /// shows as light grey, index 7).
    pub fn effective_bg(&self) -> Color {
        if self.reverse {
            match self.fg {
                Color::Default => Color::Indexed(7),
                other => other,
            }
        } else {
            self.bg
        }
    }

    /// The colour the glyph is drawn in (see `effective_bg`)
    pub fn effective_fg(&self) -> Color {
        if self.reverse {
            match self.bg {
                Color::Default => Color::Indexed(0),
                other => other,
            }
        } else {
            self.fg
        }
    }

    /// Whether the text stands out by brightness: bold, a bright indexed
    /// colour, or a light RGB colour. Turbo Vision style dialogs mark the
    /// focused control this way (bright white on the control's colour).
    /// Bright black (index 8) is dark grey, the colour of disabled and
    /// dimmed text, so it does not count.
    pub fn is_bright_fg(&self) -> bool {
        self.bold
            || match self.effective_fg() {
                Color::Indexed(n) => (9..=15).contains(&n),
                Color::Rgb(r, g, b) => {
                    (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 >= 160
                }
                Color::Default => false,
            }
    }

    /// Attributes a blank cell takes when erased: xterm keeps only the
    /// background (back colour erase), so a dialog's `EL`/`ED` fill shows in
    /// the dialog's colour.
    pub fn erase_attrs(&self) -> Attrs {
        Attrs {
            bg: self.effective_bg(),
            ..Attrs::default()
        }
    }

    /// Apply the parameters of a `CSI ... m` sequence. `groups` yields one
    /// slice per semicolon-separated parameter, with colon subparameters
    /// inside the slice (vte's `Params::iter`). No parameters means reset.
    pub fn apply_sgr<'a, I>(&mut self, groups: I)
    where
        I: IntoIterator<Item = &'a [u16]>,
    {
        let mut groups = groups.into_iter();
        let mut seen_any = false;
        while let Some(group) = groups.next() {
            seen_any = true;
            let code = group.first().copied().unwrap_or(0);
            match code {
                0 => *self = Attrs::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                // `4:0` turns underline off; any other subparameter is a style
                4 => self.underline = group.get(1).map_or(true, |&style| style != 0),
                7 => self.reverse = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                24 => self.underline = false,
                27 => self.reverse = false,
                30..=37 => self.fg = Color::Indexed((code - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((code - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let color = if group.len() > 1 {
                        extended_color_from_subparams(&group[1..])
                    } else {
                        extended_color_from_groups(&mut groups)
                    };
                    if let Some(color) = color {
                        if code == 38 {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                    }
                }
                _ => {}
            }
        }
        if !seen_any {
            *self = Attrs::default();
        }
    }
}

/// `38:5:n`, `38:2:r:g:b` or the ITU form `38:2:<colourspace>:r:g:b`
fn extended_color_from_subparams(sub: &[u16]) -> Option<Color> {
    match sub {
        [5, n, ..] => Some(Color::Indexed(*n as u8)),
        [2, _, r, g, b, ..] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
        [2, r, g, b] => Some(Color::Rgb(*r as u8, *g as u8, *b as u8)),
        _ => None,
    }
}

/// `38;5;n` or `38;2;r;g;b`: the colour is spread over the following
/// parameters, which are consumed here.
fn extended_color_from_groups<'a, I>(groups: &mut I) -> Option<Color>
where
    I: Iterator<Item = &'a [u16]>,
{
    let mut next = || groups.next().and_then(|g| g.first().copied());
    match next()? {
        5 => Some(Color::Indexed(next()? as u8)),
        2 => {
            let r = next()? as u8;
            let g = next()? as u8;
            let b = next()? as u8;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(attrs: &mut Attrs, groups: &[&[u16]]) {
        attrs.apply_sgr(groups.iter().copied());
    }

    #[test]
    fn basic_colours_and_flags() {
        let mut a = Attrs::default();
        apply(&mut a, &[&[1], &[31], &[42]]);
        assert_eq!(a.fg, Color::Indexed(1));
        assert_eq!(a.bg, Color::Indexed(2));
        assert!(a.bold);
        apply(&mut a, &[&[22], &[39], &[49]]);
        assert_eq!(a, Attrs::default());
        apply(&mut a, &[&[97], &[104], &[7], &[4]]);
        assert_eq!(a.fg, Color::Indexed(15));
        assert_eq!(a.bg, Color::Indexed(12));
        assert!(a.reverse && a.underline);
        apply(&mut a, &[&[27], &[24]]);
        assert!(!a.reverse && !a.underline);
    }

    #[test]
    fn reset_forms() {
        let mut a = Attrs::default();
        apply(&mut a, &[&[31], &[0]]);
        assert_eq!(a, Attrs::default());
        apply(&mut a, &[&[31]]);
        apply(&mut a, &[]);
        assert_eq!(a, Attrs::default());
        apply(&mut a, &[&[0], &[40], &[37]]);
        assert_eq!(a.bg, Color::Indexed(0));
        assert_eq!(a.fg, Color::Indexed(7));
    }

    #[test]
    fn extended_colours_semicolon_and_colon_forms() {
        let mut a = Attrs::default();
        apply(
            &mut a,
            &[&[38], &[5], &[208], &[48], &[2], &[10], &[20], &[30]],
        );
        assert_eq!(a.fg, Color::Indexed(208));
        assert_eq!(a.bg, Color::Rgb(10, 20, 30));
        apply(&mut a, &[&[38, 2, 0, 1, 2, 3], &[48, 5, 9]]);
        assert_eq!(a.fg, Color::Rgb(1, 2, 3));
        assert_eq!(a.bg, Color::Indexed(9));
        apply(&mut a, &[&[38, 2, 4, 5, 6]]);
        assert_eq!(a.fg, Color::Rgb(4, 5, 6));
        // Parameters after an extended colour are still applied
        apply(&mut a, &[&[38], &[5], &[1], &[1]]);
        assert!(a.bold);
        assert_eq!(a.fg, Color::Indexed(1));
    }

    #[test]
    fn underline_subparams() {
        let mut a = Attrs::default();
        apply(&mut a, &[&[4, 3]]);
        assert!(a.underline);
        apply(&mut a, &[&[4, 0]]);
        assert!(!a.underline);
    }

    #[test]
    fn effective_colours_and_brightness() {
        let mut a = Attrs::default();
        apply(&mut a, &[&[7]]);
        assert_eq!(a.effective_bg(), Color::Indexed(7));
        assert_eq!(a.effective_fg(), Color::Indexed(0));
        apply(&mut a, &[&[0], &[34], &[47], &[7]]);
        assert_eq!(a.effective_bg(), Color::Indexed(4));
        assert_eq!(a.effective_fg(), Color::Indexed(7));
        apply(&mut a, &[&[0], &[97]]);
        assert!(a.is_bright_fg());
        apply(&mut a, &[&[0], &[30], &[1]]);
        assert!(a.is_bright_fg());
        apply(&mut a, &[&[0], &[30]]);
        assert!(!a.is_bright_fg());
        apply(&mut a, &[&[0], &[90]]);
        assert!(!a.is_bright_fg());
        apply(&mut a, &[&[0], &[38, 2, 250, 250, 250]]);
        assert!(a.is_bright_fg());
    }

    #[test]
    fn erase_keeps_only_background() {
        let mut a = Attrs::default();
        apply(&mut a, &[&[1], &[31], &[44]]);
        let e = a.erase_attrs();
        assert_eq!(e.bg, Color::Indexed(4));
        assert_eq!(e.fg, Color::Default);
        assert!(!e.bold);
        apply(&mut a, &[&[0], &[32], &[7]]);
        assert_eq!(a.erase_attrs().bg, Color::Indexed(2));
    }
}
