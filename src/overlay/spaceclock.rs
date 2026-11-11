//! Deep Space Clock – die Zeit, wie sie in Mission Control läuft:
//!
//! ```text
//!  DEEP SPACE CLOCK ──────────────── ● UTC     ╭──────╮
//!  14:32:07                                     │ ⟋ ⟍  │  Zifferblatt: Erdtag (Cyan)
//!  MI 11.11.2026 · TAG 315 · UNIX 1794…         ╰──────╯  und Marssol (Orange)
//!  JD            2461356.10564
//!  LST           17:48:21 · 9.99° E
//!  MARS MTC      09:41:12 · MSD 54112
//!  CURIOSITY     SOL 5071 · 08:17:44 LMST
//!  PERSEVERANCE  SOL 2036 · 03:12:09 LMST
//! ```
//!
//! Alles wird lokal berechnet (siehe [`super::astro`]) – kein Netzwerk.

use super::astro::{self, hms, ROVERS};
use super::canvas::{Canvas, Image, Layer};
use super::flight::{CYAN, DIM, SOFT};
use super::hud;
use super::text::{Color, Fonts, Line, Weight, WHITE};
use super::time::{DateTime, WEEKDAYS_SHORT};
use crate::config::DeepSpaceClockWidget;

pub const MARS: Color = [0xFB, 0x8C, 0x3C, 255];

/// Mindestbreite des Panels (Außenmaß bei Skalierung 1).
pub const WIDTH: f32 = 500.0;
const ROWS_Y: f32 = 86.0;
const RING: f32 = 90.0;
const LABEL_W: f32 = 116.0;

#[derive(Debug, Clone, PartialEq)]
pub struct SpaceClock {
    utc: String,
    date: String,
    rows: Vec<(&'static str, String, Color)>,
    /// Anteil des Erdtags (UTC) und des Marssols (MTC), 0 … 1 – für das Zifferblatt.
    earth: f32,
    mars: f32,
}

fn clock((h, m, s): (u32, u32, u32)) -> String {
    format!("{h:02}:{m:02}:{s:02}")
}

pub fn content(cfg: &DeepSpaceClockWidget, now: i64) -> SpaceClock {
    let t = now as f64;
    let d = DateTime::from_unix(now);
    let doy = (now - DateTime::new(d.year, 1, 1, 0, 0, 0).to_unix()) / 86_400 + 1;
    let date = format!("{} {:02}.{:02}.{} · TAG {doy} · UNIX {now}", WEEKDAYS_SHORT[d.weekday as usize], d.day, d.month, d.year);

    let mut rows = vec![("JD", format!("{:.5}", astro::julian_date(t)), SOFT)];
    match cfg.longitude {
        Some(lon) => {
            let side = if lon < 0.0 { 'W' } else { 'E' };
            rows.push(("LST", format!("{} · {:.2}° {side}", clock(hms(astro::lst_hours(t, lon))), lon.abs()), SOFT));
        }
        None => rows.push(("GMST", clock(hms(astro::gmst_hours(t))), SOFT)),
    }
    let mtc = astro::mtc_hours(t);
    if cfg.mars {
        rows.push(("MARS MTC", format!("{} · MSD {}", clock(hms(mtc)), astro::mars_sol_date(t).floor() as i64), MARS));
        for r in &ROVERS {
            let sol = astro::mission_sol(t, r.lon_east, r.landing);
            rows.push((r.name, format!("SOL {sol} · {} LMST", clock(hms(astro::lmst_hours(t, r.lon_east)))), SOFT));
        }
    }
    SpaceClock {
        utc: clock((d.hour, d.minute, d.second)),
        date,
        rows,
        earth: (now.rem_euclid(86_400)) as f32 / 86_400.0,
        mars: if cfg.mars { (mtc / 24.0) as f32 } else { -1.0 },
    }
}

/// `w` = Außenbreite des Panels (mindestens [`WIDTH`]), bei Skalierung 1.
pub fn render(f: &SpaceClock, fonts: &Fonts, s: f32, w: f32) -> Image {
    let iw = hud::inner(w);
    // Unten mindestens bis unter die Legende des Zifferblatts.
    let h = (ROWS_Y + hud::ROW_H * f.rows.len() as f32 - 4.0).max(RING + 18.0);
    let mut c = hud::panel(w, h + 2.0 * hud::PAD, s);
    let ring_x = (iw - RING) * s;
    let edge = ring_x - 16.0 * s;
    hud::header(&mut c, fonts, s, (0.0, edge), 0.0, ("DEEP SPACE CLOCK", CYAN), &[("UTC", CYAN)]);

    let big = Line::new(42.0, Weight::Bold).tabular().tracking(0.02).span(&f.utc, WHITE);
    c.text(Layer::Fore, fonts, &big, s, -2.0 * s, 18.0 * s);
    let date = Line::new(9.5, Weight::Bold).tracking(0.12).span(&f.date, DIM);
    c.text(Layer::Fore, fonts, &date, s, 0.0, 66.0 * s);

    let mut y = ROWS_Y * s;
    for (label, value, color) in &f.rows {
        hud::row(&mut c, fonts, s, 0.0, y, LABEL_W, edge, label, value, *color);
        y += hud::ROW_H * s;
    }

    // ── Zifferblatt: 24 Stunden, Mitternacht oben; Erde außen, Mars innen ──
    let r = RING / 2.0 * s;
    let center = (ring_x + r, r);
    let hud_c = |a: u8| [CYAN[0], CYAN[1], CYAN[2], a];
    c.ring(Layer::Fore, center, r, 1.0, hud_c(170));
    for i in 0..96 {
        let a = (i as f32 / 96.0 * 360.0).to_radians();
        let major = i % 4 == 0;
        let inner = r - if i % 24 == 0 { 7.0 } else if major { 4.5 } else { 2.5 } * s;
        let p = |rad: f32| (center.0 + a.sin() * rad, center.1 - a.cos() * rad);
        c.line(Layer::Fore, p(inner), p(r), 1.0, hud_c(if major { 170 } else { 90 }));
    }
    let hand = |c: &mut Canvas, frac: f32, len: f32, width: f32, color: Color| {
        let a = frac * std::f32::consts::TAU;
        let tip = (center.0 + a.sin() * len, center.1 - a.cos() * len);
        c.line(Layer::Fore, center, tip, width, color);
        c.disc(Layer::Fore, tip, 2.6 * s, color);
    };
    if f.mars >= 0.0 {
        c.ring(Layer::Fore, center, r * 0.62, 1.0, [MARS[0], MARS[1], MARS[2], 110]);
        hand(&mut c, f.mars, r * 0.62, 1.8 * s, MARS);
    }
    hand(&mut c, f.earth, r - 10.0 * s, 1.8 * s, CYAN);
    c.disc(Layer::Fore, center, 3.0 * s, WHITE);
    for (i, label) in ["00", "06", "12", "18"].into_iter().enumerate() {
        let a = (i as f32 * 90.0).to_radians();
        let l = hud::tiny(label, DIM);
        let lw = super::text::width(fonts, &l, s);
        let rad = r - 15.0 * s;
        c.text(Layer::Fore, fonts, &l, s, center.0 + a.sin() * rad - lw / 2.0, center.1 - a.cos() * rad - 5.5 * s);
    }
    // Legende unter dem Zifferblatt
    let mut lx = ring_x + 4.0 * s;
    let ly = RING * s + 7.0 * s;
    let legend: &[(&str, Color)] = if f.mars >= 0.0 { &[("ERDE", CYAN), ("MARS", MARS)] } else { &[("ERDE", CYAN)] };
    for (label, color) in legend {
        c.disc(Layer::Fore, (lx + 3.0 * s, ly + 5.0 * s), 3.0 * s, *color);
        lx += 10.0 * s + c.text(Layer::Fore, fonts, &hud::tiny(label, DIM), s, lx + 9.0 * s, ly) + 12.0 * s;
    }

    c.finish(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_config() {
        let mut cfg = DeepSpaceClockWidget::default();
        let now = 1_794_450_900;
        let f = content(&cfg, now);
        assert_eq!(f.utc, "02:35:00");
        assert!(f.date.starts_with("DO 12.11.2026 · TAG 316"), "{}", f.date);
        assert_eq!(f.rows.len(), 5);
        assert_eq!(f.rows[1].0, "GMST");
        cfg.longitude = Some(-71.06);
        cfg.mars = false;
        let f = content(&cfg, now);
        assert_eq!(f.rows.len(), 2);
        assert!(f.rows[1].1.ends_with("71.06° W"), "{}", f.rows[1].1);
        assert!(f.mars < 0.0);
    }

    #[test]
    fn renders() {
        let fonts = Fonts::new();
        let img = render(&content(&DeepSpaceClockWidget::default(), 1_794_450_900), &fonts, 1.0, WIDTH);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        assert!(img.glow.is_empty());
    }
}
