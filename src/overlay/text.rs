//! Text für die Widgets: Orbitron (eingebettet, SIL Open Font License), gerastert mit `ab_glyph`.

use super::canvas::{Canvas, Image, Layer};
use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, VariableFont};

static ORBITRON: &[u8] = include_bytes!("../../assets/Orbitron.ttf");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Medium,
    Bold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Farbe als RGBA (sRGB, Alpha = Deckkraft).
pub type Color = [u8; 4];

pub const WHITE: Color = [255, 255, 255, 255];

#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub text: String,
    pub color: Color,
}

/// Eine Zeile Text in einheitlicher Größe; Größen und Abstände in Pixeln bei Skalierung 1 (1080p).
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
    pub size: f32,
    pub weight: Weight,
    /// Zusätzlicher Zeichenabstand in Vielfachen der Schriftgröße.
    pub tracking: f32,
    /// Ziffern gleich breit (Uhr, Countdown springen sonst hin und her).
    pub tabular: bool,
    /// Abstand zur vorigen Zeile.
    pub gap: f32,
}

impl Line {
    pub fn new(size: f32, weight: Weight) -> Self {
        Self { spans: Vec::new(), size, weight, tracking: 0.0, tabular: false, gap: 0.0 }
    }

    pub fn span(mut self, text: impl Into<String>, color: Color) -> Self {
        self.spans.push(Span { text: text.into(), color });
        self
    }

    pub fn tracking(mut self, t: f32) -> Self {
        self.tracking = t;
        self
    }

    pub fn tabular(mut self) -> Self {
        self.tabular = true;
        self
    }

    pub fn gap(mut self, g: f32) -> Self {
        self.gap = g;
        self
    }
}

pub struct Fonts {
    medium: FontRef<'static>,
    bold: FontRef<'static>,
}

impl Fonts {
    pub fn new() -> Self {
        let load = |weight: f32| {
            let mut f = FontRef::try_from_slice(ORBITRON).expect("eingebettete Schrift ist gültig");
            f.set_variation(b"wght", weight);
            f
        };
        Self { medium: load(500.0), bold: load(700.0) }
    }

    pub fn get(&self, w: Weight) -> &FontRef<'static> {
        match w {
            Weight::Medium => &self.medium,
            Weight::Bold => &self.bold,
        }
    }
}

/// Zeichen, die Orbitron nicht kennt, durch ähnliche ersetzen.
fn glyph(font: &FontRef<'static>, c: char) -> GlyphId {
    let id = font.glyph_id(c);
    if id.0 != 0 || c == ' ' {
        return id;
    }
    let alt = match c {
        '–' | '—' | '−' => '-',
        '·' | '•' | '…' => '.',
        _ => '?',
    };
    font.glyph_id(alt)
}

pub struct Placed {
    pub id: GlyphId,
    pub x: f32,
    pub color: Color,
}

pub struct Laid {
    pub glyphs: Vec<Placed>,
    pub width: f32,
    pub ascent: f32,
    pub height: f32,
    pub px: f32,
}

pub fn layout(fonts: &Fonts, line: &Line, scale: f32) -> Laid {
    let font = fonts.get(line.weight);
    let px = line.size * scale;
    let sf = font.as_scaled(PxScale::from(px));
    let track = line.tracking * px;
    let digit_w = if line.tabular { ('0'..='9').map(|c| sf.h_advance(font.glyph_id(c))).fold(0.0, f32::max) } else { 0.0 };
    let mut glyphs = Vec::new();
    let mut x = 0.0f32;
    let mut prev: Option<GlyphId> = None;
    for span in &line.spans {
        for c in span.text.chars() {
            let id = glyph(font, c);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let adv = sf.h_advance(id);
            if line.tabular && c.is_ascii_digit() {
                glyphs.push(Placed { id, x: x + (digit_w - adv) / 2.0, color: span.color });
                x += digit_w;
            } else {
                glyphs.push(Placed { id, x, color: span.color });
                x += adv;
            }
            x += track;
            prev = Some(id);
        }
    }
    let width = if glyphs.is_empty() { 0.0 } else { x - track };
    Laid { glyphs, width, ascent: sf.ascent(), height: sf.ascent() - sf.descent(), px }
}

/// Breite einer Zeile in Pixeln.
pub fn width(fonts: &Fonts, line: &Line, scale: f32) -> f32 {
    layout(fonts, line, scale).width
}

/// Zeilen untereinander setzen und mit Schatten rastern (Uhr).
pub fn render(fonts: &Fonts, lines: &[Line], align: Align, scale: f32) -> Image {
    let laid: Vec<Laid> = lines.iter().map(|l| layout(fonts, l, scale)).collect();
    let content_w = laid.iter().map(|l| l.width).fold(0.0, f32::max).ceil();
    let content_h = lines.iter().zip(&laid).map(|(l, d)| l.gap * scale + d.height).sum::<f32>().ceil();
    let mut c = Canvas::new(content_w as u32, content_h as u32, scale);
    let mut y = 0.0;
    for (l, d) in lines.iter().zip(&laid) {
        y += l.gap * scale;
        let x = match align {
            Align::Left => 0.0,
            Align::Center => ((content_w - d.width) / 2.0).round(),
            Align::Right => content_w - d.width,
        };
        c.text(Layer::Fore, fonts, l, scale, x, y);
        y += d.height;
    }
    c.finish(scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_coverage() {
        let f = Fonts::new();
        // Alles, was die Widgets anzeigen können, soll ohne Ersatzzeichen auskommen (außer – · …).
        for c in "0123456789:-+.,/()ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÜ ".chars() {
            assert_ne!(f.medium.glyph_id(c).0, 0, "Zeichen {c:?} fehlt in Orbitron");
        }
        for c in "–·…".chars() {
            assert_ne!(glyph(&f.medium, c).0, 0);
        }
    }

    #[test]
    fn tabular_digits_keep_width() {
        let f = Fonts::new();
        let w = |t: &str| width(&f, &Line::new(48.0, Weight::Bold).tabular().span(t, WHITE), 1.0);
        assert_eq!(w("11:11:11"), w("00:00:08"));
    }

    #[test]
    fn renders_something() {
        let f = Fonts::new();
        let img = render(&f, &[Line::new(40.0, Weight::Bold).span("T- 01:02:03", WHITE)], Align::Right, 1.0);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        assert!(img.width > 2 * img.pad && img.height > 2 * img.pad);
        assert!(img.rgba.chunks_exact(4).any(|p| p[0] > 200 && p[3] > 200), "weiße Schrift vorhanden");
        assert!(img.rgba.chunks_exact(4).all(|p| p[0] <= p[3]), "vormultipliziert");
        assert!(img.glow.is_empty());
    }
}
