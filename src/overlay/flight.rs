//! Flight Control als Mission-Control-Modul:
//!
//! ```text
//!  ┌ VEHICLE TRACK ┐  NEXT LAUNCH ─────────── ● GO     ╭─────╮
//!  │  (Raketen-    │  T- 13:34:30                       │PATCH│
//!  │   Scan)       │  ▬▬▬▬▬▬▬▬▲┄┄┄┄┄┄┄┄┄ T-24H … T-0     ╰─────╯
//!  │               │  VEHICLE  FALCON 9
//!  │               │  MISSION  USSF-627
//!  └               ┘  PAD      SLC-40 / CAPE CANAVERAL SFS
//!                     T-ZERO   DO 12.11. 03:35
//! ```
//!
//! Das Raketenfoto wird nicht eingefügt, sondern als Sensor-Scan gezeichnet: nur die Kanten, in Cyan,
//! mit Scanlines und weich in das Video ausgeblendet – auf der Hintergrund-Ebene ohne Schatten.
//! Der Mission-Patch sitzt in einem Instrumenten-Ring; ohne Patch zeigt der Ring den Ziel-Orbit, fehlt beides, entfällt er.
//!
//! Countdown-Effekte (nur bei minutengenauer Startzeit): ab T-10:00 „TERMINAL COUNT“, in den letzten
//! 10 Sekunden bernsteinfarbene Ziffern, die im Sekundentakt aufleuchten, bei T-0 „LIFTOFF“ mit Blitz.
//! Das Leuchten steckt in einer eigenen Ebene, die der Shader pro Bild ein- und ausblendet.

use super::canvas::{box_blur, resample, Canvas, Image, Layer, Px};
use super::launch::{Asset, View, Visual};
use super::text::{self, Color, Fonts, Line, Weight, WHITE};
use super::time::{MONTHS, WEEKDAYS_SHORT};
use crate::platform::sys;
use std::collections::HashMap;
use std::sync::Arc;

pub(super) const CYAN: Color = [0x22, 0xD3, 0xEE, 255];
pub(super) const GREEN: Color = [0x34, 0xD3, 0x99, 255];
pub(super) const AMBER: Color = [0xF5, 0x9E, 0x0B, 255];
pub(super) const RED: Color = [0xF8, 0x71, 0x71, 255];
pub(super) const DIM: Color = [255, 255, 255, 140];
pub(super) const SOFT: Color = [255, 255, 255, 230];
pub(super) const HAIRLINE: Color = [255, 255, 255, 70];

// Maße bei Skalierung 1 (1080p).
const W: f32 = 700.0;
const H: f32 = 236.0;
const SCAN_W: f32 = 170.0;
/// Bildfläche im Rahmen: links Platz fürs Höhenband, oben fürs Label.
const SCAN_INSET: (f32, f32, f32, f32) = (18.0, 22.0, 6.0, 8.0);
const RING: f32 = 88.0;
const COL_X: f32 = SCAN_W + 14.0;
const LABEL_W: f32 = 84.0;

/// Animierter Effekt, den der Shader pro Bild berechnet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fx {
    None,
    /// Letzte 10 Sekunden: Aufleuchten im Sekundentakt.
    Pulse,
    /// Abgehoben: kräftiger Blitz bei T-0, der langsam ausklingt.
    Liftoff { net: i64 },
}

/// Stärke des Leuchtens zum Zeitpunkt `t` (Unix-Sekunden mit Bruchteil).
pub fn flash(fx: Fx, t: f64) -> f32 {
    match fx {
        Fx::None => 0.0,
        Fx::Pulse => 0.25 + 0.75 * (-(t.fract() as f32) * 4.0).exp(),
        Fx::Liftoff { net } => {
            let dt = (t - net as f64) as f32;
            if dt < 0.0 {
                0.0
            } else {
                0.3 + 1.4 * (-dt / 1.5).exp()
            }
        }
    }
}

/// Alles, was das Modul zeigt. Unverändert → kein neues Rastern.
#[derive(Debug, Clone, PartialEq)]
pub struct Flight {
    header: (String, Color),
    status: Option<(String, Color)>,
    live: bool,
    count: String,
    count_color: Color,
    glow: Option<Color>,
    /// Fortschritt der letzten 24 Stunden vor dem Start (0 … 1).
    rail: f32,
    rows: Vec<(&'static str, String)>,
    orbit: String,
    scan_label: &'static str,
    photo: Option<Asset>,
    patch: Option<Asset>,
    pub fx: Fx,
}

fn two(n: u32) -> String {
    format!("{n:02}")
}

fn status_color(status: &str) -> Color {
    match status {
        "Go" | "Success" => GREEN,
        "In Flight" => CYAN,
        "Hold" | "Failure" | "Partial Failure" => RED,
        _ => AMBER,
    }
}

/// "T- 1D 02:14:37" vor dem Start, "T+ 00:03:12" danach.
pub fn countdown(net: i64, now: i64) -> String {
    let d = net - now;
    let sign = if d > 0 { "T-" } else { "T+" };
    let s = d.unsigned_abs();
    let (days, h, m, sec) = (s / 86_400, s / 3600 % 24, s / 60 % 60, s % 60);
    if days > 0 {
        format!("{sign} {days}D {h:02}:{m:02}:{sec:02}")
    } else {
        format!("{sign} {h:02}:{m:02}:{sec:02}")
    }
}

pub fn content(view: View, now: i64) -> Flight {
    let placeholder = |label: &'static str, text: &str| Flight {
        header: ("NEXT LAUNCH".into(), CYAN),
        status: None,
        live: false,
        count: "T- --:--:--".into(),
        count_color: WHITE,
        glow: None,
        rail: 0.0,
        rows: vec![("STATUS", text.into())],
        orbit: String::new(),
        scan_label: label,
        photo: None,
        patch: None,
        fx: Fx::None,
    };
    let v = match view {
        View::Waiting => return placeholder("ACQUIRING", "AWAITING TELEMETRY"),
        View::NoSignal => return placeholder("NO SIGNAL", "NO SIGNAL"),
        View::Launch(v) => v,
    };
    let l = &v.launch;
    let d = l.net - now;
    // Effekte nur, wenn die Zeit wirklich feststeht.
    let tracked = l.exact && !matches!(l.status.as_str(), "TBD" | "TBC");
    let liftoff = tracked && d <= 0 && d > -60;
    let terminal = tracked && d > 0 && d <= 600;
    let final_ten = tracked && d > 0 && d <= 10;

    let header = if liftoff {
        ("LIFTOFF".to_string(), GREEN)
    } else if terminal {
        ("TERMINAL COUNT".to_string(), AMBER)
    } else {
        ("NEXT LAUNCH".to_string(), CYAN)
    };
    let local = sys::local_time(l.net);
    let count = if l.precise || d <= 0 {
        countdown(l.net, now)
    } else {
        format!("NET {}. {}", local.day, MONTHS[local.month as usize - 1].chars().take(3).collect::<String>())
    };
    let (count_color, glow, fx) = if final_ten {
        ([255, 214, 130, 255], Some(AMBER), Fx::Pulse)
    } else if liftoff {
        ([170, 255, 205, 255], Some(GREEN), Fx::Liftoff { net: l.net })
    } else {
        (WHITE, None, Fx::None)
    };
    let when = if l.precise {
        format!("{} {}.{}. {}:{}", WEEKDAYS_SHORT[local.weekday as usize], two(local.day), two(local.month), two(local.hour), two(local.minute))
    } else {
        format!("NET {}.{}.{}", two(local.day), two(local.month), local.year)
    };
    let site: Vec<&str> = [l.pad.as_str(), l.location.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
    Flight {
        header,
        status: (!l.status.is_empty()).then(|| (l.status.to_uppercase(), status_color(&l.status))),
        live: l.webcast,
        count,
        count_color,
        glow,
        rail: if d <= 0 { 1.0 } else { (1.0 - d as f32 / 86_400.0).clamp(0.0, 1.0) },
        rows: vec![
            ("VEHICLE", l.rocket.to_uppercase()),
            ("MISSION", l.mission.to_uppercase()),
            ("PAD", site.join(" / ").to_uppercase()),
            ("T-ZERO", when),
        ],
        orbit: l.orbit.to_uppercase(),
        scan_label: "VEHICLE TRACK",
        photo: v.photo.clone(),
        patch: v.patch.clone(),
        fx,
    }
}

/// Bearbeitete Bilder (Scan, Patch) je Adresse und Größe – Rastern passiert jede Sekunde, das Bearbeiten nicht.
#[derive(Default)]
pub struct Cache {
    map: HashMap<(String, u32, u32), Arc<Vec<Px>>>,
}

impl Cache {
    fn get(&mut self, v: &Visual, w: u32, h: u32, make: impl FnOnce(&Visual, u32, u32) -> Vec<Px>) -> Arc<Vec<Px>> {
        let key = (v.url.clone(), w, h);
        if let Some(p) = self.map.get(&key) {
            return p.clone();
        }
        if self.map.len() >= 8 {
            self.map.clear();
        }
        let p = Arc::new(make(v, w, h));
        self.map.insert(key, p.clone());
        p
    }
}

/// Text auf `max` Pixel kürzen (mit „…“).
pub(super) fn fit(fonts: &Fonts, base: &Line, s: &str, scale: f32, max: f32) -> String {
    let line = |t: &str| base.clone().span(t, WHITE);
    if text::width(fonts, &line(s), scale) <= max {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let t: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if text::width(fonts, &line(&t), scale) <= max {
            return t;
        }
    }
    String::new()
}

pub fn render(f: &Flight, fonts: &Fonts, scale: f32, cache: &mut Cache) -> Image {
    let s = scale;
    let mut c = Canvas::new((W * s).ceil() as u32, (H * s).ceil() as u32, s);
    let hud = |a: u8| [CYAN[0], CYAN[1], CYAN[2], a];

    // ── Raketen-Scan (Hintergrund-Ebene), genau im Inneren des Rahmens ──
    let (il, it, ir, ib) = SCAN_INSET;
    let (sx, sy) = ((il * s).round(), (it * s).round());
    let (sw, sh) = (((SCAN_W - il - ir) * s).round() as u32, ((H - it - ib) * s).round() as u32);
    match &f.photo {
        Some(p) => {
            let scan = cache.get(&p.0, sw, sh, |v, w, h| scan(v, w, h, s));
            c.image(Layer::Back, sx as i32, sy as i32, sw, sh, &scan);
        }
        None => {
            // Kein Bild: ruhiges Raster, damit die Fläche nicht leer wirkt.
            let step = 21.0 * s;
            let (ex, ey) = (sx + sw as f32, sy + sh as f32);
            let mut x = sx + step / 2.0;
            while x < ex {
                c.line(Layer::Back, (x, sy), (x, ey), 1.0, hud(28));
                x += step;
            }
            let mut y = sy + step / 2.0;
            while y < ey {
                c.line(Layer::Back, (sx, y), (ex, y), 1.0, hud(28));
                y += step;
            }
            let label = Line::new(11.0, Weight::Bold).tracking(0.2).span(f.scan_label, hud(200));
            let lw = text::width(fonts, &label, s);
            c.text(Layer::Fore, fonts, &label, s, sx + (sw as f32 - lw) / 2.0, sy + sh as f32 / 2.0 - 7.0 * s);
        }
    }

    // ── HUD-Rahmen um den Scan ──
    let (x0, y0, x1, y1) = (0.5, 0.5, SCAN_W * s, H * s - 0.5);
    let arm = 14.0 * s;
    let lw = (1.6 * s).max(1.0);
    for (ax, ay, dx, dy) in [(x0, y0, 1.0, 1.0), (x1, y0, -1.0, 1.0), (x0, y1, 1.0, -1.0), (x1, y1, -1.0, -1.0)] {
        c.line(Layer::Fore, (ax, ay), (ax + dx * arm, ay), lw, hud(210));
        c.line(Layer::Fore, (ax, ay), (ax, ay + dy * arm), lw, hud(210));
    }
    // Höhenband
    let mut i = 0;
    let mut ty = 24.0 * s;
    while ty < (H - 20.0) * s {
        let len = if i % 5 == 0 { 8.0 } else { 4.0 } * s;
        c.line(Layer::Fore, (4.0 * s, ty), (4.0 * s + len, ty), 1.0, hud(120));
        ty += 8.0 * s;
        i += 1;
    }
    if f.photo.is_some() {
        let label = Line::new(9.0, Weight::Bold).tracking(0.12).span(f.scan_label, hud(190));
        c.text(Layer::Fore, fonts, &label, s, 18.0 * s, 6.0 * s);
    }

    // ── Kopfzeile: Titel, Haarlinie, Status ──
    let x = COL_X * s;
    // Den Ring gibt es nur, wenn er etwas Echtes zeigt (Patch oder Ziel-Orbit); sonst nutzt der Text die Breite.
    let ring = f.patch.is_some() || !f.orbit.is_empty();
    let ring_x = (W - RING) * s;
    let edge = if ring { ring_x - 16.0 * s } else { W * s };
    let head = Line::new(14.0, Weight::Bold).tracking(0.12).span(&f.header.0, f.header.1);
    let head_w = c.text(Layer::Fore, fonts, &head, s, x, 2.0 * s);
    let mut right = edge;
    let mid_y = 11.0 * s;
    let pill = |c: &mut Canvas, label: &str, color: Color, right: &mut f32| {
        let t = Line::new(14.0, Weight::Bold).tracking(0.08).span(label, color);
        let tw = text::width(fonts, &t, s);
        let tx = *right - tw;
        c.text(Layer::Fore, fonts, &t, s, tx, 2.0 * s);
        c.disc(Layer::Fore, (tx - 9.0 * s, mid_y), 4.5 * s, color);
        *right = tx - 24.0 * s;
    };
    if let Some((st, col)) = &f.status {
        pill(&mut c, st, *col, &mut right);
    }
    if f.live {
        pill(&mut c, "LIVE", RED, &mut right);
    }
    if right > x + head_w + 24.0 * s {
        c.line(Layer::Fore, (x + head_w + 12.0 * s, mid_y), (right, mid_y), 1.0, HAIRLINE);
    }

    // ── Countdown (passt sich der Breite an, z. B. mit Tagen) ──
    let avail = edge - x;
    let mut size = 54.0;
    let count_line = |size: f32, color: Color| Line::new(size, Weight::Bold).tabular().tracking(0.02).span(&f.count, color);
    while size > 30.0 && text::width(fonts, &count_line(size, WHITE), s) > avail {
        size -= 2.0;
    }
    let cy = 22.0 * s + (54.0 - size) * 0.6 * s;
    c.text(Layer::Fore, fonts, &count_line(size, f.count_color), s, x - 2.0 * s, cy);
    if let Some(g) = f.glow {
        c.text(Layer::Glow, fonts, &count_line(size, g), s, x - 2.0 * s, cy);
    }

    // ── Schiene: die letzten 24 Stunden ──
    let ry = 96.0 * s;
    let rw = edge - 8.0 * s - x;
    c.line(Layer::Fore, (x, ry), (x + rw, ry), 1.0, [255, 255, 255, 60]);
    let fx = x + rw * f.rail;
    if f.rail > 0.0 {
        c.line(Layer::Fore, (x, ry), (fx, ry), 3.0 * s, f.header.1);
    }
    for i in 0..=24 {
        let tx = x + rw * i as f32 / 24.0;
        let len = if i % 6 == 0 { 5.0 } else { 2.0 } * s;
        c.line(Layer::Fore, (tx, ry - len), (tx, ry), 1.0, [255, 255, 255, 110]);
    }
    c.triangle(Layer::Fore, [(fx - 5.0 * s, ry + 9.0 * s), (fx + 5.0 * s, ry + 9.0 * s), (fx, ry + 3.0 * s)], f.header.1);
    let tiny = |t: &str| Line::new(9.0, Weight::Bold).tracking(0.1).span(t, DIM);
    c.text(Layer::Fore, fonts, &tiny("T-24H"), s, x, ry + 8.0 * s);
    let t0 = tiny("T-0");
    c.text(Layer::Fore, fonts, &t0, s, x + rw - text::width(fonts, &t0, s), ry + 8.0 * s);

    // ── Datenzeilen ──
    let value_base = Line::new(15.0, Weight::Bold).tracking(0.05);
    let value_max = W * s - (x + LABEL_W * s);
    let mut y = 126.0 * s;
    for (label, value) in &f.rows {
        c.text(Layer::Fore, fonts, &Line::new(10.0, Weight::Bold).tracking(0.14).span(*label, DIM), s, x, y + 3.0 * s);
        let v = fit(fonts, &value_base, value, s, value_max);
        c.text(Layer::Fore, fonts, &value_base.clone().span(v, SOFT), s, x + LABEL_W * s, y);
        y += 26.0 * s;
    }

    // ── Instrumenten-Ring: Mission-Patch oder Ziel-Orbit (aus den Startdaten) ──
    if ring {
        let r = RING / 2.0 * s;
        let center = (ring_x + r, 4.0 * s + r);
        c.ring(Layer::Fore, center, r, 1.0, hud(170));
        for i in 0..72 {
            let a = (i as f32 * 5.0).to_radians();
            let inner = r - if i % 6 == 0 { 5.0 } else { 2.5 } * s;
            c.line(Layer::Fore, (center.0 + a.cos() * inner, center.1 + a.sin() * inner), (center.0 + a.cos() * r, center.1 + a.sin() * r), 1.0, hud(150));
        }
        match &f.patch {
            Some(p) => {
                let side = ((RING - 16.0) * s).round() as u32;
                let img = cache.get(&p.0, side, side, |v, w, _| patch(v, w));
                c.image(Layer::Fore, (center.0 - side as f32 / 2.0).round() as i32, (center.1 - side as f32 / 2.0).round() as i32, side, side, &img);
            }
            None => {
                let label = tiny("ORBIT");
                let lw = text::width(fonts, &label, s);
                c.text(Layer::Fore, fonts, &label, s, center.0 - lw / 2.0, center.1 - 20.0 * s);
                let orbit = Line::new(17.0, Weight::Bold).tracking(0.06).span(&f.orbit, SOFT);
                let ow = text::width(fonts, &orbit, s);
                c.text(Layer::Fore, fonts, &orbit, s, center.0 - ow / 2.0, center.1 - 7.0 * s);
            }
        }
    }

    c.finish(s)
}

/// Raketenfoto → Sensor-Scan: Kanten in Cyan (helle Kerne weiß), Scanlines, weich ausgeblendet.
fn scan(v: &Visual, w: u32, h: u32, scale: f32) -> Vec<Px> {
    // Ausschnitt mit dem Seitenverhältnis des Ziels, mittig (wie "cover").
    let (vw, vh) = (v.width as f32, v.height as f32);
    let k = (w as f32 / vw).max(h as f32 / vh);
    let (cw, ch) = (((w as f32 / k).round() as u32).clamp(1, v.width), ((h as f32 / k).round() as u32).clamp(1, v.height));
    let (cx, cy) = ((v.width - cw) / 2, (v.height - ch) / 2);
    let mut crop = Vec::with_capacity((cw * ch * 4) as usize);
    for y in cy..cy + ch {
        let row = ((y * v.width + cx) * 4) as usize;
        crop.extend_from_slice(&v.rgba[row..row + (cw * 4) as usize]);
    }
    let px = resample(&crop, cw, ch, w, h);
    let (wu, hu) = (w as usize, h as usize);
    let mut g: Vec<f32> = px.iter().map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2]).collect();
    box_blur(&mut g, wu, hu, 1);

    // Sobel
    let at = |x: isize, y: isize| g[(y.clamp(0, hu as isize - 1) as usize) * wu + x.clamp(0, wu as isize - 1) as usize];
    let mut e = vec![0f32; wu * hu];
    for y in 0..hu as isize {
        for x in 0..wu as isize {
            let gx = at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x - 1, y) - at(x - 1, y + 1);
            let gy = at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1) - at(x - 1, y - 1) - 2.0 * at(x, y - 1) - at(x + 1, y - 1);
            e[y as usize * wu + x as usize] = (gx * gx + gy * gy).sqrt();
        }
    }
    let mut sorted = e.clone();
    sorted.sort_unstable_by(f32::total_cmp);
    let p99 = sorted[(sorted.len() as f32 * 0.99) as usize].max(1e-4);
    for v in &mut e {
        *v = (*v / p99).clamp(0.0, 1.0).powf(1.2);
    }
    let mut glow = e.clone();
    let r = (1.5 * scale).round().max(1.0) as usize;
    box_blur(&mut glow, wu, hu, r);
    box_blur(&mut glow, wu, hu, r);

    let period = (3.0 * scale).round().max(3.0) as usize;
    let thick = scale.round().max(1.0) as usize;
    let cyan = [CYAN[0] as f32 / 255.0, CYAN[1] as f32 / 255.0, CYAN[2] as f32 / 255.0];
    let mut out = vec![[0f32; 4]; wu * hu];
    for y in 0..hu {
        let fy = (y as f32 / (hu as f32 * 0.12)).min(1.0) * ((hu - y) as f32 / (hu as f32 * 0.16)).min(1.0);
        let scanline = if y % period < thick { 0.45 } else { 1.0 };
        for x in 0..wu {
            let i = y * wu + x;
            let fx = (x as f32 / (wu as f32 * 0.14)).min(1.0) * ((wu - x) as f32 / (wu as f32 * 0.14)).min(1.0);
            let mask = (fx * fy).max(0.0).powf(1.3);
            let a = (e[i] * 0.95 + glow[i] * 0.9 * 0.5).min(1.0) * scanline * mask * 0.85;
            let core = ((e[i] - 0.55) * 2.5).clamp(0.0, 1.0);
            let col = [0, 1, 2].map(|k| cyan[k] * (1.0 - core) + core);
            out[i] = [col[0] * a, col[1] * a, col[2] * a, a];
        }
    }
    out
}

/// Patch einpassen und rund freistellen (weiche Kante), leicht gedämpft – wie hinter Glas.
fn patch(v: &Visual, side: u32) -> Vec<Px> {
    let k = side as f32 / v.width.max(v.height) as f32;
    let (pw, ph) = (((v.width as f32 * k).round() as u32).max(1), ((v.height as f32 * k).round() as u32).max(1));
    let small = resample(&v.rgba, v.width, v.height, pw, ph);
    let mut out = vec![[0f32; 4]; (side * side) as usize];
    let (ox, oy) = ((side - pw.min(side)) / 2, (side - ph.min(side)) / 2);
    let r = side as f32 / 2.0;
    for y in 0..ph.min(side) {
        for x in 0..pw.min(side) {
            let (tx, ty) = (x + ox, y + oy);
            let d = ((tx as f32 + 0.5 - r).powi(2) + (ty as f32 + 0.5 - r).powi(2)).sqrt();
            let m = (r + 0.5 - d).clamp(0.0, 1.0) * 0.94;
            out[(ty * side + tx) as usize] = small[(y * pw + x) as usize].map(|c| c * m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::launch::{Launch, LaunchView};

    fn launch(net: i64, status: &str, exact: bool) -> View {
        View::Launch(Box::new(LaunchView {
            launch: Launch {
                id: "x".into(),
                rocket: "Falcon 9".into(),
                mission: "USSF-627".into(),
                orbit: "LEO".into(),
                net,
                precise: true,
                exact,
                status: status.into(),
                pad: "SLC-40".into(),
                location: "Cape Canaveral SFS".into(),
                webcast: false,
                image: String::new(),
            },
            photo: None,
            patch: None,
        }))
    }

    #[test]
    fn countdown_format() {
        assert_eq!(countdown(1000 + 2 * 86_400 + 3723, 1000), "T- 2D 01:02:03");
        assert_eq!(countdown(1000 + 59, 1000), "T- 00:00:59");
        assert_eq!(countdown(1000, 1000 + 192), "T+ 00:03:12");
    }

    #[test]
    fn phases() {
        let now = 1_000_000;
        let f = content(launch(now + 3600, "Go", true), now);
        assert_eq!((f.header.0.as_str(), f.fx), ("NEXT LAUNCH", Fx::None));
        let f = content(launch(now + 300, "Go", true), now);
        assert_eq!((f.header.0.as_str(), f.fx), ("TERMINAL COUNT", Fx::None));
        let f = content(launch(now + 7, "Go", true), now);
        assert_eq!((f.header.0.as_str(), f.fx), ("TERMINAL COUNT", Fx::Pulse));
        assert!(f.glow.is_some());
        let f = content(launch(now - 5, "Go", true), now);
        assert_eq!(f.header.0, "LIFTOFF");
        assert_eq!(f.fx, Fx::Liftoff { net: now - 5 });
        // Ungenaue oder unbestätigte Zeiten: kein Spektakel.
        assert_eq!(content(launch(now + 7, "TBC", true), now).fx, Fx::None);
        assert_eq!(content(launch(now + 7, "Go", false), now).header.0, "NEXT LAUNCH");
    }

    #[test]
    fn flash_curves() {
        assert!(flash(Fx::Pulse, 100.0) > 0.99);
        assert!(flash(Fx::Pulse, 100.9) < 0.3);
        assert_eq!(flash(Fx::Liftoff { net: 100 }, 99.0), 0.0);
        assert!(flash(Fx::Liftoff { net: 100 }, 100.0) > 1.5);
        assert!(flash(Fx::Liftoff { net: 100 }, 130.0) < 0.35);
        assert_eq!(flash(Fx::None, 5.0), 0.0);
    }

    #[test]
    fn renders_panel() {
        let fonts = Fonts::new();
        let mut cache = Cache::default();
        let now = 1_000_000;
        let img = render(&content(launch(now + 5, "Go", true), now), &fonts, 1.0, &mut cache);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        assert_eq!(img.glow.len(), img.rgba.len(), "Countdown-Leuchten vorhanden");
        let img = render(&content(View::Waiting, now), &fonts, 1.5, &mut cache);
        assert!(img.glow.is_empty());
        assert!(img.width as f32 >= W * 1.5);
    }
}

