//! Kleine 2D-Zeichenfläche für die Widgets (CPU, vormultipliziertes Alpha, kantengeglättet).
//!
//! Drei Ebenen:
//! - `Back`: Bildmaterial, das mit dem Video verschmilzt (wirft keinen Schatten),
//! - `Fore`: Schrift und HUD-Linien (bekommen einen weichen Schatten für die Lesbarkeit),
//! - `Glow`: Leuchten, das der Shader im Takt ein- und ausblendet (Countdown-Effekt).

use super::text::{self, Color, Fonts, Line};
use ab_glyph::{point, Font};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Back,
    Fore,
    Glow,
}

/// Vormultipliziertes RGBA als f32.
pub type Px = [f32; 4];

pub fn premul(c: Color) -> Px {
    let a = c[3] as f32 / 255.0;
    [c[0] as f32 / 255.0 * a, c[1] as f32 / 255.0 * a, c[2] as f32 / 255.0 * a, a]
}

/// Fertiges Bild (RGBA8, vormultipliziert). `pad` = Rand für den Schatten auf jeder Seite.
/// `glow` hat dieselbe Größe oder ist leer.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pad: u32,
    pub rgba: Vec<u8>,
    pub glow: Vec<u8>,
}

pub struct Canvas {
    pub w: u32,
    pub h: u32,
    pub pad: u32,
    /// Ursprung der Zeichenkoordinaten im Puffer (Schattenrand plus ggf. Innenabstand eines Panels).
    ox: f32,
    oy: f32,
    /// Nur innerhalb dieses Rechtecks zeichnen (Pufferkoordinaten, x0, y0, x1, y1) – gilt für Formen.
    clip: Option<(f32, f32, f32, f32)>,
    back: Vec<Px>,
    fore: Vec<Px>,
    glow: Option<Vec<Px>>,
}

impl Canvas {
    /// Fläche für Inhalt der Größe `w` × `h` plus Schattenrand; Koordinaten beziehen sich auf den Inhalt.
    pub fn new(w: u32, h: u32, scale: f32) -> Self {
        let pad = shadow_radius(scale) as u32 * 3 + 2;
        let (w, h) = (w + 2 * pad, h + 2 * pad);
        let n = (w * h) as usize;
        Self { w, h, pad, ox: pad as f32, oy: pad as f32, clip: None, back: vec![[0.0; 4]; n], fore: vec![[0.0; 4]; n], glow: None }
    }

    /// Zeichenkoordinaten ab jetzt relativ zu `(x, y)` (in Pixeln, bezogen auf den Inhalt ohne Schattenrand).
    pub fn set_origin(&mut self, x: f32, y: f32) {
        self.ox = self.pad as f32 + x.round();
        self.oy = self.pad as f32 + y.round();
    }

    /// Formen (Linien, Kreise, Dreiecke, Rechtecke) ab jetzt nur innerhalb von `r` zeichnen; `None` hebt das auf.
    pub fn set_clip(&mut self, r: Option<(f32, f32, f32, f32)>) {
        self.clip = r.map(|(x0, y0, x1, y1)| (x0 + self.ox, y0 + self.oy, x1 + self.ox, y1 + self.oy));
    }

    /// Wo der aktuelle Ursprung im fertigen Bild liegt (inkl. Schattenrand).
    pub fn origin(&self) -> (u32, u32) {
        (self.ox as u32, self.oy as u32)
    }

    fn buf(&mut self, l: Layer) -> &mut Vec<Px> {
        match l {
            Layer::Back => &mut self.back,
            Layer::Fore => &mut self.fore,
            Layer::Glow => {
                let n = (self.w * self.h) as usize;
                self.glow.get_or_insert_with(|| vec![[0.0; 4]; n])
            }
        }
    }

    /// Ein Pixel (Inhaltskoordinaten) mit Deckung `cov` überblenden.
    #[inline]
    fn put(buf: &mut [Px], w: u32, h: u32, x: i32, y: i32, c: Px, cov: f32) {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || cov <= 0.0 {
            return;
        }
        let d = &mut buf[(y as u32 * w + x as u32) as usize];
        let k = cov.min(1.0);
        let inv = 1.0 - c[3] * k;
        *d = [c[0] * k + d[0] * inv, c[1] * k + d[1] * inv, c[2] * k + d[2] * inv, c[3] * k + d[3] * inv];
    }

    /// Fläche über ein Abstandsfeld füllen: `dist(x, y)` < 0 = innen; Kante 1 px weich.
    fn shape(&mut self, l: Layer, bounds: (f32, f32, f32, f32), color: Color, dist: impl Fn(f32, f32) -> f32) {
        let (ox, oy, w, h) = (self.ox, self.oy, self.w, self.h);
        let c = premul(color);
        let clip = self.clip.unwrap_or((0.0, 0.0, w as f32, h as f32));
        let buf = self.buf(l);
        let (x0, y0, x1, y1) = bounds;
        let ys = ((y0 + oy - 1.0).floor().max(clip.1.floor()) as i32)..=((y1 + oy + 1.0).ceil().min(clip.3.ceil() - 1.0) as i32);
        let xs = ((x0 + ox - 1.0).floor().max(clip.0.floor()) as i32)..=((x1 + ox + 1.0).ceil().min(clip.2.ceil() - 1.0) as i32);
        for y in ys {
            for x in xs.clone() {
                let d = dist(x as f32 + 0.5 - ox, y as f32 + 0.5 - oy);
                Self::put(buf, w, h, x, y, c, (0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// Linie mit Breite `width` (gerade Enden).
    pub fn line(&mut self, l: Layer, a: (f32, f32), b: (f32, f32), width: f32, color: Color) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(1e-3);
        let (ux, uy) = (dx / len, dy / len);
        let hw = width / 2.0;
        let bounds = (a.0.min(b.0) - hw, a.1.min(b.1) - hw, a.0.max(b.0) + hw, a.1.max(b.1) + hw);
        self.shape(l, bounds, color, |px, py| {
            let (rx, ry) = (px - a.0, py - a.1);
            let along = rx * ux + ry * uy;
            let across = (rx * -uy + ry * ux).abs();
            (across - hw).max(-along).max(along - len)
        });
    }

    /// Achsparalleles Rechteck (`x0, y0, x1, y1`).
    pub fn rect(&mut self, l: Layer, r: (f32, f32, f32, f32), color: Color) {
        let (x0, y0, x1, y1) = r;
        self.shape(l, r, color, |px, py| (x0 - px).max(px - x1).max(y0 - py).max(py - y1));
    }

    /// HUD-Ecken (wie bei Flight Control) um ein Rechteck.
    pub fn corners(&mut self, l: Layer, r: (f32, f32, f32, f32), arm: f32, width: f32, color: Color) {
        let (x0, y0, x1, y1) = r;
        for (ax, ay, dx, dy) in [(x0, y0, 1.0, 1.0), (x1, y0, -1.0, 1.0), (x0, y1, 1.0, -1.0), (x1, y1, -1.0, -1.0)] {
            self.line(l, (ax, ay), (ax + dx * arm, ay), width, color);
            self.line(l, (ax, ay), (ax, ay + dy * arm), width, color);
        }
    }

    pub fn ring(&mut self, l: Layer, c: (f32, f32), r: f32, width: f32, color: Color) {
        let bounds = (c.0 - r - width, c.1 - r - width, c.0 + r + width, c.1 + r + width);
        self.shape(l, bounds, color, |px, py| (((px - c.0).powi(2) + (py - c.1).powi(2)).sqrt() - r).abs() - width / 2.0);
    }

    pub fn disc(&mut self, l: Layer, c: (f32, f32), r: f32, color: Color) {
        self.shape(l, (c.0 - r, c.1 - r, c.0 + r, c.1 + r), color, |px, py| ((px - c.0).powi(2) + (py - c.1).powi(2)).sqrt() - r);
    }

    pub fn triangle(&mut self, l: Layer, p: [(f32, f32); 3], color: Color) {
        let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| {
            let (ex, ey) = (b.0 - a.0, b.1 - a.1);
            let len = (ex * ex + ey * ey).sqrt().max(1e-3);
            ((x - a.0) * ey - (y - a.1) * ex) / len
        };
        // Umlaufsinn angleichen, damit "innen" immer negativ ist.
        let ccw = edge(p[0], p[1], p[2].0, p[2].1) > 0.0;
        let bounds = (
            p.iter().map(|q| q.0).fold(f32::MAX, f32::min),
            p.iter().map(|q| q.1).fold(f32::MAX, f32::min),
            p.iter().map(|q| q.0).fold(f32::MIN, f32::max),
            p.iter().map(|q| q.1).fold(f32::MIN, f32::max),
        );
        self.shape(l, bounds, color, |x, y| {
            let d = [edge(p[0], p[1], x, y), edge(p[1], p[2], x, y), edge(p[2], p[0], x, y)];
            let d = d.map(|v| if ccw { -v } else { v });
            d[0].max(d[1]).max(d[2])
        });
    }

    /// Vormultipliziertes Bild (`w` × `h`) an `(x, y)` einsetzen.
    pub fn image(&mut self, l: Layer, x: i32, y: i32, w: u32, h: u32, data: &[Px]) {
        let (ox, oy, cw, ch) = (self.ox as i32, self.oy as i32, self.w, self.h);
        let buf = self.buf(l);
        for iy in 0..h {
            for ix in 0..w {
                let c = data[(iy * w + ix) as usize];
                if c[3] > 0.0 {
                    Self::put(buf, cw, ch, x + ox + ix as i32, y + oy + iy as i32, c, 1.0);
                }
            }
        }
    }

    /// Eine Textzeile; `y` = Oberkante der Zeile. Liefert die Breite.
    pub fn text(&mut self, l: Layer, fonts: &Fonts, line: &Line, scale: f32, x: f32, y: f32) -> f32 {
        let laid = text::layout(fonts, line, scale);
        let font = fonts.get(line.weight);
        let (ox, w, h) = (self.ox, self.w, self.h);
        let baseline = (y + self.oy + laid.ascent).round();
        let buf = self.buf(l);
        for g in &laid.glyphs {
            let Some(outline) = font.outline_glyph(g.id.with_scale_and_position(laid.px, point((x + ox + g.x).round(), baseline))) else {
                continue;
            };
            let b = outline.px_bounds();
            let c = premul(g.color);
            outline.draw(|gx, gy, cov| {
                Self::put(buf, w, h, b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, c, cov);
            });
        }
        laid.width
    }

    /// Weicher, dauerhafter Schein hinter einer Textzeile (Hintergrund-Ebene, also ohne Schatten):
    /// Schrift unscharf zeichnen, Deckkraft × `strength`.
    #[allow(clippy::too_many_arguments)]
    pub fn halo(&mut self, fonts: &Fonts, line: &Line, scale: f32, x: f32, y: f32, radius: f32, strength: f32, color: Color) {
        let laid = text::layout(fonts, line, scale);
        let font = fonts.get(line.weight);
        let (w, h) = (self.w as usize, self.h as usize);
        let mut cov = vec![0.0f32; w * h];
        let baseline = (y + self.oy + laid.ascent).round();
        for g in &laid.glyphs {
            let Some(outline) = font.outline_glyph(g.id.with_scale_and_position(laid.px, point((x + self.ox + g.x).round(), baseline))) else {
                continue;
            };
            let b = outline.px_bounds();
            outline.draw(|gx, gy, c| {
                let (px, py) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                    let v = &mut cov[py as usize * w + px as usize];
                    *v = (*v + c).min(1.0);
                }
            });
        }
        let r = (radius * scale).max(1.0) as usize;
        for _ in 0..3 {
            box_blur(&mut cov, w, h, r);
        }
        let c = premul(color);
        for (i, v) in cov.iter().enumerate() {
            if *v > 0.002 {
                Self::put(&mut self.back, self.w, self.h, (i % w) as i32, (i / w) as i32, c, v * strength);
            }
        }
    }

    /// Ebenen zusammenführen: Hintergrund, Schatten der Vordergrund-Ebene, Vordergrund.
    pub fn finish(self, scale: f32) -> Image {
        let r = shadow_radius(scale);
        let mut shadow: Vec<f32> = self.fore.iter().map(|p| p[3]).collect();
        for _ in 0..2 {
            box_blur(&mut shadow, self.w as usize, self.h as usize, r);
        }
        let mut rgba = Vec::with_capacity(self.fore.len() * 4);
        for ((f, b), s) in self.fore.iter().zip(&self.back).zip(&shadow) {
            // Hintergrund, darüber Schatten (schwarz), darüber Vordergrund.
            let sa = (s * 1.1).min(0.85);
            let under = [b[0] * (1.0 - sa), b[1] * (1.0 - sa), b[2] * (1.0 - sa), sa + b[3] * (1.0 - sa)];
            let inv = 1.0 - f[3];
            for k in 0..4 {
                rgba.push(to_u8(f[k] + under[k] * inv));
            }
        }
        let glow = match self.glow {
            Some(mut g) => {
                // Leuchten weich ausbreiten (pro Kanal).
                let (w, h) = (self.w as usize, self.h as usize);
                let rad = (8.0 * scale).max(2.0) as usize;
                for k in 0..4 {
                    let mut ch: Vec<f32> = g.iter().map(|p| p[k]).collect();
                    for _ in 0..2 {
                        box_blur(&mut ch, w, h, rad);
                    }
                    for (p, v) in g.iter_mut().zip(ch) {
                        p[k] = v;
                    }
                }
                g.iter().flat_map(|p| p.map(|v| to_u8(v * 1.6))).collect()
            }
            None => Vec::new(),
        };
        Image { width: self.w, height: self.h, pad: self.pad, rgba, glow }
    }
}

pub fn shadow_radius(scale: f32) -> usize {
    (4.0 * scale).max(1.0) as usize
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Box-Weichzeichner (waagerecht, dann senkrecht) mit gleitender Summe.
pub fn box_blur(buf: &mut [f32], w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
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
            for (i, t) in tmp.iter_mut().take(len).enumerate() {
                *t = sum * norm;
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

/// Vormultipliziertes RGBA8-Bild flächengemittelt auf `tw` × `th` bringen (nur verkleinern sinnvoll).
pub fn resample(src: &[u8], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<Px> {
    let mut out = vec![[0f32; 4]; (tw * th) as usize];
    let (fx, fy) = (sw as f32 / tw as f32, sh as f32 / th as f32);
    for ty in 0..th {
        let y0 = (ty as f32 * fy) as u32;
        let y1 = (((ty + 1) as f32 * fy).ceil() as u32).clamp(y0 + 1, sh);
        for tx in 0..tw {
            let x0 = (tx as f32 * fx) as u32;
            let x1 = (((tx + 1) as f32 * fx).ceil() as u32).clamp(x0 + 1, sw);
            let mut acc = [0f32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * sw + x) * 4) as usize;
                    for k in 0..4 {
                        acc[k] += src[i + k] as f32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32 * 255.0;
            out[(ty * tw + tx) as usize] = acc.map(|v| v / n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_cover_their_area() {
        let mut c = Canvas::new(40, 40, 1.0);
        c.disc(Layer::Fore, (15.0, 15.0), 5.0, [255, 255, 255, 255]);
        let p = c.pad;
        let at = |c: &Canvas, x: u32, y: u32| c.fore[((y + p) * c.w + x + p) as usize][3];
        assert!(at(&c, 15, 15) > 0.99);
        assert!(at(&c, 25, 25) < 0.01);
        c.ring(Layer::Fore, (30.0, 30.0), 6.0, 2.0, [255, 0, 0, 255]);
        assert!(at(&c, 36, 30) > 0.9, "auf dem Ring");
        assert!(at(&c, 30, 30) < 0.01, "Mitte frei");
    }

    #[test]
    fn resample_averages() {
        let src = [255u8, 255, 255, 255, 0, 0, 0, 0].repeat(2); // 2x2: links weiß, rechts leer
        let out = resample(&src, 2, 2, 1, 1);
        assert!((out[0][3] - 0.5).abs() < 0.01);
    }
}
