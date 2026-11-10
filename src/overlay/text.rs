//! Text für die Widgets: Orbitron (eingebettet, SIL Open Font License) wird auf der CPU gerastert –
//! nur wenn sich der Inhalt ändert (höchstens einmal pro Sekunde) – und als fertiges Bild hochgeladen.
//! Ein weicher Schatten hält die Schrift auf hellen wie dunklen Videos lesbar.

use ab_glyph::{point, Font, FontRef, GlyphId, PxScale, ScaleFont, VariableFont};

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

/// Fertiges Bild mit vormultipliziertem Alpha. `pad` = Rand für den Schatten (auf jeder Seite).
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pad: u32,
    pub rgba: Vec<u8>,
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

    fn get(&self, w: Weight) -> &FontRef<'static> {
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
        '·' | '•' => '.',
        '…' => '.',
        _ => '?',
    };
    font.glyph_id(alt)
}

struct Placed {
    id: GlyphId,
    x: f32,
    color: Color,
}

struct Laid {
    glyphs: Vec<Placed>,
    width: f32,
    ascent: f32,
    height: f32,
    gap: f32,
    px: f32,
    weight: Weight,
}

fn layout(fonts: &Fonts, line: &Line, scale: f32) -> Laid {
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
    Laid { glyphs, width, ascent: sf.ascent(), height: sf.ascent() - sf.descent(), gap: line.gap * scale, px, weight: line.weight }
}

/// Zeilen untereinander setzen und mit Schatten rastern.
pub fn render(fonts: &Fonts, lines: &[Line], align: Align, scale: f32) -> Image {
    let laid: Vec<Laid> = lines.iter().map(|l| layout(fonts, l, scale)).collect();
    let blur = (4.0 * scale).max(1.0) as usize;
    let pad = (blur * 3 + 2) as u32;
    let content_w = laid.iter().map(|l| l.width).fold(0.0, f32::max).ceil() as u32;
    let content_h = laid.iter().map(|l| l.gap + l.height).sum::<f32>().ceil() as u32;
    let (w, h) = ((content_w + 2 * pad).max(1), (content_h + 2 * pad).max(1));

    // Farbe, vormultipliziert, als f32 für sauberes Überblenden.
    let mut px = vec![[0f32; 4]; (w * h) as usize];
    let mut y = pad as f32;
    for l in &laid {
        y += l.gap;
        let x0 = pad as f32
            + match align {
                Align::Left => 0.0,
                Align::Center => ((content_w as f32 - l.width) / 2.0).round(),
                Align::Right => content_w as f32 - l.width,
            };
        let baseline = (y + l.ascent).round();
        let font = fonts.get(l.weight);
        for g in &l.glyphs {
            let Some(outline) = font.outline_glyph(g.id.with_scale_and_position(l.px, point(x0 + g.x, baseline))) else { continue };
            let b = outline.px_bounds();
            let [r, gr, bl, a] = g.color.map(|c| c as f32 / 255.0);
            outline.draw(|gx, gy, cov| {
                let (ix, iy) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if ix < 0 || iy < 0 || ix >= w as i32 || iy >= h as i32 {
                    return;
                }
                let k = cov.clamp(0.0, 1.0) * a;
                let d = &mut px[(iy as u32 * w + ix as u32) as usize];
                let inv = 1.0 - k;
                *d = [r * k + d[0] * inv, gr * k + d[1] * inv, bl * k + d[2] * inv, k + d[3] * inv];
            });
        }
        y += l.height;
    }

    // Schatten: Alpha der Schrift weichzeichnen und schwarz darunterlegen.
    let mut shadow: Vec<f32> = px.iter().map(|p| p[3]).collect();
    for _ in 0..2 {
        box_blur(&mut shadow, w as usize, h as usize, blur);
    }
    let mut rgba = Vec::with_capacity(px.len() * 4);
    for (p, s) in px.iter().zip(&shadow) {
        let sa = (s * 1.1).min(0.85) * (1.0 - p[3]);
        let a = p[3] + sa;
        rgba.extend_from_slice(&[to_u8(p[0]), to_u8(p[1]), to_u8(p[2]), to_u8(a)]);
    }
    Image { width: w, height: h, pad, rgba }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Box-Weichzeichner (waagerecht, dann senkrecht) mit gleitender Summe.
fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 {
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    let mut tmp = vec![0f32; w.max(h)];
    let mut pass = |len: usize, count: usize, idx: &dyn Fn(usize, usize) -> usize, buf: &mut [f32]| {
        for line in 0..count {
            let mut sum = 0.0;
            for i in 0..=r.min(len - 1) {
                sum += buf[idx(line, i)];
            }
            for i in 0..len {
                tmp[i] = sum * norm;
                if i + r + 1 < len {
                    sum += buf[idx(line, i + r + 1)];
                }
                if i >= r {
                    sum -= buf[idx(line, i - r)];
                }
            }
            for (i, v) in tmp.iter().take(len).enumerate() {
                buf[idx(line, i)] = *v;
            }
        }
    };
    pass(w, h, &|y, x| y * w + x, buf);
    pass(h, w, &|x, y| y * w + x, buf);
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
        let w = |t: &str| layout(&f, &Line::new(48.0, Weight::Bold).tabular().span(t, WHITE), 1.0).width;
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
    }
}
