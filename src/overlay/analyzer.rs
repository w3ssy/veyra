//! Signal Analyzer – der Systemsound auf einem Messgerät mit grün nachleuchtendem Phosphor-Schirm:
//!
//! ```text
//!  SIGNAL ANALYZER ───────────────────── ● LIVE
//!  ┌                                         ┐
//!     ▂▃▅▇█▇▅▄▃▂▂▃▄▅▄▃▂▁▁                       Spektrum (48 Bänder, Spitzenwerte),
//!                                              Oszilloskop (getriggert) oder XY (Goniometer)
//!  └                                         ┘
//!   50   100        1K        10K   16K
//!   SPECTRUM · 48.0 KHZ · PEAK −12 DBFS
//! ```
//!
//! Rahmen, Raster und Beschriftung werden wie alle Widgets höchstens einmal pro Sekunde gerastert.
//! Die Leuchtspur selbst zeichnet [`Phosphor`] pro Bild auf der CPU und legt sie in die Leucht-Ebene
//! des Widgets – reines Licht, das sich zum Video addiert, mit echtem Nachleuchten wie auf einer Röhre.
//! Ist nichts zu hören und die Spur verblasst, wird nichts mehr gezeichnet.

use super::canvas::{box_blur, Canvas, Image, Layer};
use super::flight::{CYAN, DIM, GREEN};
use super::hud;
use super::text::{self, Fonts, Line, Weight};
use crate::audio::scope::{self, Scope, BANDS, DB_CEIL, DB_FLOOR};
use crate::config::AnalyzerMode;
use std::time::Instant;

const W: f32 = 460.0;
const SCREEN_Y: f32 = 26.0;
const SCREEN_H: f32 = 136.0;
const H: f32 = 196.0;
/// P31-Phosphor (Oszilloskop-Grün), linear 0 … 1.
const PHOSPHOR: [f32; 3] = [0.32, 1.0, 0.56];
const TRACE: [u8; 4] = [0x52, 0xFF, 0x8F, 255];

#[derive(Debug, Clone, PartialEq)]
pub struct Analyzer {
    mode: AnalyzerMode,
    live: bool,
    rate: u32,
    peak: Option<i32>,
}

pub fn content(mode: AnalyzerMode, s: &Scope) -> Analyzer {
    Analyzer {
        mode,
        live: s.active,
        rate: s.rate,
        peak: (s.active && s.peak_db.is_finite()).then(|| s.peak_db.round().max(-99.0) as i32),
    }
}

impl Analyzer {
    pub fn mode(&self) -> AnalyzerMode {
        self.mode
    }
}

/// Schirm (Leuchtspur) im fertigen Bild: x, y, Breite, Höhe in Pixeln.
pub type Screen = (u32, u32, u32, u32);

/// Frequenz → Anteil der Schirmbreite (logarithmisch, wie die Bänder).
fn freq_x(hz: f32) -> f32 {
    ((hz / scope::LOW_HZ).ln() / (scope::HIGH_HZ / scope::LOW_HZ).ln()).clamp(0.0, 1.0)
}

fn db_y(db: f32) -> f32 {
    1.0 - (db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)
}

pub fn render(a: &Analyzer, fonts: &Fonts, s: f32) -> (Image, Screen) {
    let mut c = Canvas::new((W * s).ceil() as u32, (H * s).ceil() as u32, s);
    let (status, color) = if a.live { ("LIVE", GREEN) } else { ("NO CARRIER", DIM) };
    hud::header(&mut c, fonts, s, (0.0, W * s), 2.0 * s, ("SIGNAL ANALYZER", CYAN), &[(status, color)]);

    let (sx0, sy0) = (0.0, (SCREEN_Y * s).round());
    let (sw, sh) = ((W * s).round(), (SCREEN_H * s).round());
    let (sx1, sy1) = (sx0 + sw, sy0 + sh);
    // Dunkles Glas hinter der Spur, damit sie auch über hellen Videos lesbar bleibt.
    c.rect(Layer::Back, (sx0, sy0, sx1, sy1), [2, 10, 6, 150]);
    let grid = |a: u8| [TRACE[0], TRACE[1], TRACE[2], a];
    let hline = |c: &mut Canvas, y: f32, a: u8| c.line(Layer::Back, (sx0, y), (sx1, y), 1.0, grid(a));
    let vline = |c: &mut Canvas, x: f32, a: u8| c.line(Layer::Back, (x, sy0), (x, sy1), 1.0, grid(a));
    let axis_y = sy1 + 6.0 * s;
    let label_at = |c: &mut Canvas, t: &str, x: f32, y: f32| {
        let l = hud::tiny(t, DIM);
        let w = text::width(fonts, &l, s);
        c.text(Layer::Fore, fonts, &l, s, (x - w / 2.0).clamp(sx0, sx1 - w), y);
    };
    match a.mode {
        AnalyzerMode::Spectrum => {
            let mut db = DB_CEIL;
            while db > DB_FLOOR {
                let y = sy0 + db_y(db) * sh;
                hline(&mut c, y, 40);
                if db < DB_CEIL {
                    c.text(Layer::Fore, fonts, &hud::tiny(&format!("{}", db as i32), grid(150)), s, sx0 + 4.0 * s, y + 2.0 * s);
                }
                db -= 12.0;
            }
            for (hz, label) in [(50.0, "50"), (100.0, "100"), (200.0, "200"), (500.0, "500"), (1000.0, "1K"), (2000.0, "2K"), (5000.0, "5K"), (10_000.0, "10K"), (16_000.0, "16K")] {
                let x = sx0 + freq_x(hz) * sw;
                vline(&mut c, x, if matches!(label, "100" | "1K" | "10K") { 50 } else { 22 });
                label_at(&mut c, label, x, axis_y);
            }
        }
        AnalyzerMode::Wave => {
            for i in 1..10 {
                vline(&mut c, sx0 + sw * i as f32 / 10.0, if i == 5 { 70 } else { 32 });
            }
            for i in 1..4 {
                hline(&mut c, sy0 + sh * i as f32 / 4.0, if i == 2 { 70 } else { 32 });
            }
            // Zeitbasis: 1024 Samples über die Breite, also pro Teilung …
            let div_ms = if a.rate > 0 { 1024.0 / a.rate as f32 * 100.0 } else { 0.0 };
            label_at(&mut c, &format!("{div_ms:.1} MS/DIV"), sx0 + sw / 2.0, axis_y);
            label_at(&mut c, "TRIG RISE", sx1, axis_y);
            label_at(&mut c, "CH1+CH2", sx0, axis_y);
        }
        AnalyzerMode::Xy => {
            let (cx, cy) = (sx0 + sw / 2.0, sy0 + sh / 2.0);
            let r = sh * 0.45;
            c.ring(Layer::Back, (cx, cy), r, 1.0, grid(45));
            c.ring(Layer::Back, (cx, cy), r * 0.5, 1.0, grid(28));
            hline(&mut c, cy, 28);
            c.line(Layer::Back, (cx, sy0), (cx, sy1), 1.0, grid(60));
            let d = r * std::f32::consts::FRAC_1_SQRT_2;
            c.line(Layer::Back, (cx - d, cy - d), (cx + d, cy + d), 1.0, grid(45));
            c.line(Layer::Back, (cx + d, cy - d), (cx - d, cy + d), 1.0, grid(45));
            let tag = |c: &mut Canvas, t: &str, x: f32, y: f32| c.text(Layer::Fore, fonts, &hud::tiny(t, grid(170)), s, x, y);
            tag(&mut c, "L", cx - d - 10.0 * s, cy - d - 12.0 * s);
            tag(&mut c, "R", cx + d + 4.0 * s, cy - d - 12.0 * s);
            tag(&mut c, "M", cx + 4.0 * s, sy0 + 3.0 * s);
            tag(&mut c, "+S", cx + r + 6.0 * s, cy + 2.0 * s);
            tag(&mut c, "-S", cx - r - 18.0 * s, cy + 2.0 * s);
            label_at(&mut c, "GONIOMETER · M/S", sx0 + sw / 2.0, axis_y);
        }
    }
    c.corners(Layer::Fore, (sx0 + 0.5, sy0 + 0.5, sx1 - 0.5, sy1 - 0.5), 12.0 * s, (1.6 * s).max(1.0), [CYAN[0], CYAN[1], CYAN[2], 210]);

    let mode = match a.mode {
        AnalyzerMode::Spectrum => "SPECTRUM",
        AnalyzerMode::Wave => "OSCILLOSCOPE",
        AnalyzerMode::Xy => "XY",
    };
    let rate = if a.rate > 0 { format!("{:.1} KHZ", a.rate as f32 / 1000.0) } else { "-- KHZ".into() };
    let peak = a.peak.map_or("PEAK -- DBFS".to_string(), |p| format!("PEAK {p} DBFS"));
    let info = Line::new(10.0, Weight::Bold).tracking(0.14).span(format!("{mode} · {rate} · {peak}"), DIM);
    c.text(Layer::Fore, fonts, &info, s, 0.0, (H - 13.0) * s);

    let img = c.finish(s);
    let pad = img.pad;
    let screen = (pad + sx0 as u32, pad + sy0 as u32, sw as u32, sh as u32);
    (img, screen)
}

/// Die Leuchtspur eines Analyzers auf einem Monitor (eigene Größe je Skalierung).
pub struct Phosphor {
    pub screen: Screen,
    mode: AnalyzerMode,
    /// Lichtmenge je Pixel (≥ 0, über 1 = überstrahlt).
    buf: Vec<f32>,
    bloom: Vec<f32>,
    out: Vec<u8>,
    gain: f32,
    scale: f32,
    last: Option<Instant>,
    /// Noch etwas sichtbar (Nachleuchten) → weiter zeichnen.
    lit: bool,
}

impl Phosphor {
    pub fn new(screen: Screen, mode: AnalyzerMode, scale: f32) -> Self {
        let n = (screen.2 * screen.3) as usize;
        Self { screen, mode, buf: vec![0.0; n], bloom: vec![0.0; n], out: vec![0; n * 4], gain: 1.0, scale, last: None, lit: false }
    }

    /// Passt dieser Schirm noch zu einem neu gerasterten Widget? Dann bleibt das Nachleuchten erhalten.
    pub fn fits(&self, screen: Screen, mode: AnalyzerMode) -> bool {
        self.screen == screen && self.mode == mode
    }

    /// Die zuletzt gezeichnete Spur (RGBA für die Leucht-Ebene).
    pub fn current(&self) -> &[u8] {
        &self.out
    }

    /// Braucht es ein neues Bild?
    pub fn busy(&self, s: &Scope) -> bool {
        s.active || self.lit
    }

    /// Ein Bild weiter: altes Licht abklingen lassen, neue Spur zeichnen. Liefert RGBA für die Leucht-Ebene.
    pub fn step(&mut self, s: &Scope, now: Instant) -> &[u8] {
        let dt = self.last.map_or(1.0 / 60.0, |l| now.duration_since(l).as_secs_f32()).clamp(0.001, 0.1);
        self.last = Some(now);
        let tau = match self.mode {
            AnalyzerMode::Spectrum => 0.05,
            AnalyzerMode::Wave => 0.07,
            AnalyzerMode::Xy => 0.11,
        };
        let decay = (-dt / tau).exp();
        let mut max = 0.0f32;
        for v in &mut self.buf {
            *v *= decay;
            if *v < 0.003 {
                *v = 0.0;
            }
            max = max.max(*v);
        }
        if s.active {
            // Pro Bild nur so viel neues Licht, dass der Dauerzustand unabhängig von der Bildrate ist.
            let energy = 1.0 - decay;
            match self.mode {
                AnalyzerMode::Spectrum => self.spectrum(s),
                AnalyzerMode::Wave => self.wave(s, energy),
                AnalyzerMode::Xy => self.xy(s, energy),
            }
            max = 1.0;
        }
        self.lit = max > 0.0;
        self.shade();
        &self.out
    }

    fn size(&self) -> (usize, usize) {
        (self.screen.2 as usize, self.screen.3 as usize)
    }

    /// Lichtmenge bilinear auf die vier Nachbarpixel verteilen.
    #[inline]
    fn splat(&mut self, x: f32, y: f32, e: f32) {
        let (w, h) = self.size();
        let (x, y) = (x - 0.5, y - 0.5);
        let (ix, iy) = (x.floor(), y.floor());
        let (fx, fy) = (x - ix, y - iy);
        for (dx, dy, k) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
            let (px, py) = (ix as isize + dx, iy as isize + dy);
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h {
                self.buf[py as usize * w + px as usize] += e * k;
            }
        }
    }

    /// Strahl von `a` nach `b`: dieselbe Lichtmenge pro Sample – schnelle Bewegungen bleiben dunkler, wie auf einer echten Röhre.
    fn beam(&mut self, a: (f32, f32), b: (f32, f32), e: f32) {
        let len = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let n = (len / 0.7).ceil().max(1.0) as usize;
        let per = e / n as f32;
        for i in 0..n {
            let t = (i as f32 + 0.5) / n as f32;
            self.splat(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, per);
        }
    }

    /// Verstärkung nachführen: schnell leiser, langsam lauter – damit die Spur den Schirm füllt, ohne zu pumpen.
    fn track_gain(&mut self, peak: f32) {
        let target = 0.9 / peak.max(0.03);
        let k = if target < self.gain { 0.35 } else { 0.04 };
        self.gain += (target - self.gain) * k;
    }

    fn spectrum(&mut self, s: &Scope) {
        let (w, h) = self.size();
        let bw = w as f32 / BANDS as f32;
        let gap = (2.0 * self.scale).round().max(1.0);
        let period = (4.0 * self.scale).round().max(3.0) as usize;
        let cap = (2.0 * self.scale).round().max(1.0) as usize;
        for b in 0..BANDS.min(s.bands.len()) {
            let x0 = (b as f32 * bw).round() as usize;
            let x1 = (((b + 1) as f32 * bw - gap).round() as usize).clamp(x0 + 1, w);
            let top = h - (s.bands[b].clamp(0.0, 1.0) * h as f32).round() as usize;
            let peak = h.saturating_sub((s.peaks[b].clamp(0.0, 1.0) * h as f32).round() as usize + cap);
            for y in 0..h {
                let from_bottom = h - 1 - y;
                let v = if y >= top && from_bottom % period < period - 1 {
                    // Leicht heller nach oben hin, wie übersteuerte LEDs.
                    0.55 + 0.35 * (1.0 - y as f32 / h as f32)
                } else if s.peaks[b] > 0.02 && y >= peak && y < peak + cap {
                    1.5
                } else {
                    continue;
                };
                for x in x0..x1 {
                    let p = &mut self.buf[y * w + x];
                    *p = p.max(v);
                }
            }
        }
    }

    fn wave(&mut self, s: &Scope, energy: f32) {
        let (w, h) = self.size();
        let n = s.left.len().min(s.right.len());
        if n < 2048 {
            return;
        }
        let m: Vec<f32> = s.left.iter().zip(&s.right).map(|(l, r)| (l + r) * 0.5).collect();
        // Trigger wie am echten Oszilloskop auf ein tiefpassgefiltertes Signal (~400 Hz): steilster Nulldurchgang
        // nach oben in der ersten Hälfte. Obertöne und Rauschen lassen ihn so nicht hin- und herspringen.
        let mut acc = 0.0f32;
        let lp: Vec<f32> = m.iter().map(|v| {
            acc += (v - acc) * 0.05;
            acc
        }).collect();
        let trig = (64..n / 2)
            .filter(|&i| lp[i - 1] < 0.0 && lp[i] >= 0.0)
            .max_by(|&a, &b| (lp[a] - lp[a - 1]).total_cmp(&(lp[b] - lp[b - 1])))
            .unwrap_or(0);
        let window = &m[trig..trig + n / 2];
        self.track_gain(window.iter().fold(0.0f32, |a, v| a.max(v.abs())));
        let (gain, mid) = (self.gain, h as f32 / 2.0);
        let e = 1.6 * energy * self.scale;
        let pt = |i: usize, v: f32| (i as f32 / (window.len() - 1) as f32 * w as f32, mid - v * gain * mid);
        for i in 1..window.len() {
            self.beam(pt(i - 1, window[i - 1]), pt(i, window[i]), e);
        }
    }

    fn xy(&mut self, s: &Scope, energy: f32) {
        let (w, h) = self.size();
        let n = s.left.len().min(s.right.len());
        let from = n.saturating_sub(1024);
        let (l, r) = (&s.left[from..n], &s.right[from..n]);
        // Ausschlag im Goniometer ist |L±R|/√2 – danach richtet sich die Verstärkung.
        let reach = l.iter().zip(r).fold(0.0f32, |a, (x, y)| a.max((x + y).abs()).max((x - y).abs()));
        self.track_gain(reach * std::f32::consts::FRAC_1_SQRT_2);
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let radius = h as f32 * 0.45;
        // Goniometer: Mitte (L+R) nach oben, Seite (L−R) nach rechts – Mono ist eine senkrechte Linie.
        let k = self.gain * radius * std::f32::consts::FRAC_1_SQRT_2;
        let pt = |a: f32, b: f32| (cx + (a - b) * k, cy - (a + b) * k);
        let e = 1.3 * energy * self.scale;
        for i in 1..l.len() {
            self.beam(pt(l[i - 1], r[i - 1]), pt(l[i], r[i]), e);
        }
    }

    /// Lichtmenge → Farbe: grüner Phosphor, überstrahlte Stellen werden weiß, dazu ein weicher Schein.
    fn shade(&mut self) {
        let (w, h) = self.size();
        self.bloom.copy_from_slice(&self.buf);
        let r = (3.0 * self.scale).round().max(1.0) as usize;
        box_blur(&mut self.bloom, w, h, r);
        box_blur(&mut self.bloom, w, h, r);
        for (i, (&v, &b)) in self.buf.iter().zip(&self.bloom).enumerate() {
            let core = 1.0 - (-1.8 * v).exp();
            let white = ((v - 1.0) * 0.6).clamp(0.0, 0.55);
            let halo = (b * 0.55).min(0.6);
            let px = &mut self.out[i * 4..i * 4 + 4];
            for k in 0..3 {
                let c = PHOSPHOR[k] * (core + halo) + white;
                px[k] = (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            px[3] = px[0].max(px[1]).max(px[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, pan: f32) -> Scope {
        let mut s = Scope { active: true, rate: 48_000, ..Default::default() };
        for i in 0..s.left.len() {
            let v = 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin();
            s.left[i] = v * (1.0 - pan);
            s.right[i] = v * (1.0 + pan);
        }
        s.bands = (0..BANDS).map(|b| if b == 20 { 0.9 } else { 0.1 }).collect();
        s.peaks = s.bands.clone();
        s
    }

    fn lit_columns(p: &Phosphor) -> Vec<usize> {
        let (w, h) = p.size();
        (0..w).filter(|&x| (0..h).any(|y| p.buf[y * w + x] > 0.05)).collect()
    }

    #[test]
    fn mono_xy_is_vertical() {
        let mut p = Phosphor::new((0, 0, 200, 100), AnalyzerMode::Xy, 1.0);
        let s = tone(440.0, 0.0);
        let t = Instant::now();
        for i in 0..10 {
            p.step(&s, t + std::time::Duration::from_millis(16 * i));
        }
        let cols = lit_columns(&p);
        assert!(!cols.is_empty());
        assert!(cols.iter().all(|&x| x.abs_diff(100) <= 2), "nur die Mittelsenkrechte leuchtet: {cols:?}");
        assert!(p.busy(&s));
    }

    #[test]
    fn wave_fills_width_and_fades() {
        let mut p = Phosphor::new((0, 0, 300, 100), AnalyzerMode::Wave, 1.0);
        let mut later = Instant::now();
        for _ in 0..10 {
            later += std::time::Duration::from_millis(16);
            p.step(&tone(500.0, 0.0), later);
        }
        assert!(lit_columns(&p).len() > 280, "{}", lit_columns(&p).len());
        let quiet = Scope::default();
        for _ in 0..60 {
            later += std::time::Duration::from_millis(16);
            p.step(&quiet, later);
        }
        assert!(!p.busy(&quiet), "nach einer Sekunde Stille ist der Schirm dunkel");
        assert!(p.out.iter().all(|&v| v == 0));
    }

    #[test]
    fn spectrum_bar_height() {
        let mut p = Phosphor::new((0, 0, 480, 100), AnalyzerMode::Spectrum, 1.0);
        p.step(&tone(1000.0, 0.0), Instant::now());
        let (w, _) = p.size();
        let col = |x: usize| (0..100).filter(|&y| p.buf[y * w + x] > 0.5).count();
        // Band 20 liegt bei x = 200 … 208, ~90 % hoch (abzüglich LED-Lücken); Nachbarn ~10 %.
        assert!(col(204) > 55, "{}", col(204));
        assert!(col(100) < 20, "{}", col(100));
    }

    #[test]
    fn renders_panel() {
        let fonts = Fonts::new();
        for mode in [AnalyzerMode::Spectrum, AnalyzerMode::Wave, AnalyzerMode::Xy] {
            let (img, screen) = render(&content(mode, &tone(440.0, 0.0)), &fonts, 1.25);
            assert!(screen.0 + screen.2 <= img.width && screen.1 + screen.3 <= img.height);
            assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        }
    }
}
