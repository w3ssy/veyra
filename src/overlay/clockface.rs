//! Die Uhr als Mission-Control-Herzstück – frei stehend über dem Video, ohne Glasplatte:
//!
//! ```text
//!  ┌                                                    ┐
//!  ──────────────  LOCAL TIME · UTC+01:00  ──────────────
//!                 21:38 :51                                 große Ziffern mit Cyan-Schein,
//!  |·····|·····|·····|····▲|·····|·····|·····|·····|      Sekunden kleiner in Cyan
//!  00          06          12          18          24      24-Stunden-Leiste bis „jetzt“
//!             MITTWOCH · 11. NOVEMBER 2026
//!       UTC 20:38:51 · TAG 315 / 365 · KW 46 · Q4
//!  └                                                    ┘
//! ```

use super::canvas::{Canvas, Image, Layer};
use super::flight::{CYAN, DIM, HAIRLINE, SOFT};
use super::hud;
use super::text::{self, Fonts, Line, Weight, WHITE};
use super::time::{DateTime, MONTHS, WEEKDAYS};
use crate::config::ClockWidget;

/// Breite der Leiste und Kopfzeile (bei Skalierung 1); die Ziffern stehen mittig darüber.
const W: f32 = 600.0;
/// Abstand der HUD-Ecken zum Inhalt.
const FRAME: f32 = 16.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Clock {
    hm: String,
    sec: Option<String>,
    zone: String,
    /// Anteil des Tages (Ortszeit), 0 … 1 – ohne Sekundenanzeige minutengenau, damit nur einmal pro Minute gerastert wird.
    day: f32,
    date: Option<String>,
    telemetry: Option<String>,
}

/// ISO-8601-Kalenderwoche.
pub fn iso_week(d: DateTime) -> u32 {
    let doy = |y: i32, m: u32, dd: u32| ((DateTime::new(y, m, dd, 0, 0, 0).to_unix() - DateTime::new(y, 1, 1, 0, 0, 0).to_unix()) / 86_400 + 1) as i32;
    let weeks = |y: i32| {
        let jan1 = DateTime::new(y, 1, 1, 0, 0, 0).weekday;
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        if jan1 == 4 || (leap && jan1 == 3) {
            53
        } else {
            52
        }
    };
    let wd = if d.weekday == 0 { 7 } else { d.weekday as i32 };
    let w = (doy(d.year, d.month, d.day) - wd + 10) / 7;
    if w < 1 {
        weeks(d.year - 1)
    } else if w > weeks(d.year) as i32 {
        1
    } else {
        w as u32
    }
}

/// `local` = Ortszeit zu `now` (Unix, UTC).
pub fn content(cfg: &ClockWidget, local: DateTime, now: i64) -> Clock {
    let offset_min = (local.to_unix() - now).div_euclid(60);
    let sign = if offset_min < 0 { '-' } else { '+' };
    let zone = format!("LOCAL TIME · UTC{sign}{:02}:{:02}", offset_min.abs() / 60, offset_min.abs() % 60);
    let secs = local.hour * 3600 + local.minute * 60 + if cfg.seconds { local.second } else { 0 };
    let utc = DateTime::from_unix(now);
    let year_start = DateTime::new(local.year, 1, 1, 0, 0, 0).to_unix();
    let doy = (DateTime::new(local.year, local.month, local.day, 0, 0, 0).to_unix() - year_start) / 86_400 + 1;
    let days = (DateTime::new(local.year + 1, 1, 1, 0, 0, 0).to_unix() - year_start) / 86_400;
    let utc_time = if cfg.seconds {
        format!("{:02}:{:02}:{:02}", utc.hour, utc.minute, utc.second)
    } else {
        format!("{:02}:{:02}", utc.hour, utc.minute)
    };
    Clock {
        hm: format!("{:02}:{:02}", local.hour, local.minute),
        sec: cfg.seconds.then(|| format!(":{:02}", local.second)),
        zone,
        day: secs as f32 / 86_400.0,
        date: cfg.date.then(|| format!("{} · {}. {} {}", WEEKDAYS[local.weekday as usize], local.day, MONTHS[local.month as usize - 1], local.year)),
        telemetry: cfg.date.then(|| {
            format!("UTC {utc_time} · TAG {doy} / {days} · KW {} · Q{}", iso_week(local), (local.month - 1) / 3 + 1)
        }),
    }
}

pub fn render(k: &Clock, fonts: &Fonts, s: f32) -> Image {
    let big = Line::new(100.0, Weight::Bold).tabular().tracking(0.03).span(&k.hm, WHITE);
    let sec = k.sec.as_ref().map(|t| Line::new(44.0, Weight::Bold).tabular().tracking(0.03).span(t, CYAN));
    let head = Line::new(10.0, Weight::Bold).tracking(0.3).span(&k.zone, CYAN);
    let date = k.date.as_ref().map(|t| Line::new(15.0, Weight::Medium).tracking(0.28).span(t, SOFT));
    let tele = k.telemetry.as_ref().map(|t| Line::new(9.0, Weight::Bold).tracking(0.18).span(t, DIM));

    let big_l = text::layout(fonts, &big, s);
    let sec_l = sec.as_ref().map(|l| text::layout(fonts, l, s));
    let gap = 6.0 * s;
    let digits_w = big_l.width + sec_l.as_ref().map_or(0.0, |l| gap + l.width);
    let w = (W * s).max(digits_w + 40.0 * s).round();
    let f = FRAME * s;

    // Senkrechte Abfolge (alles in Pixeln, Skalierung schon eingerechnet)
    let y_head = f;
    let y_big = y_head + 14.0 * s;
    // Orbitron hat unter den Ziffern viel Unterlänge – die Leiste rückt deshalb nah heran.
    let y_track = y_big + big_l.ascent + 14.0 * s;
    let mut y = y_track + 22.0 * s;
    let y_date = date.as_ref().map(|_| {
        let v = y + 4.0 * s;
        y = v + 22.0 * s;
        v
    });
    let y_tele = tele.as_ref().map(|_| {
        let v = y;
        y = v + 14.0 * s;
        v
    });
    let h = y + f - 4.0 * s;
    let total_w = w + 2.0 * f;
    let mut c = Canvas::new(total_w.ceil() as u32, h.ceil() as u32, s);
    let hud_c = |a: u8| [CYAN[0], CYAN[1], CYAN[2], a];
    let cx = total_w / 2.0;
    let centered = |c: &mut Canvas, l: &Line, y: f32| {
        let lw = text::width(fonts, l, s);
        c.text(Layer::Fore, fonts, l, s, (cx - lw / 2.0).round(), y)
    };

    // HUD-Ecken um die ganze Uhr
    c.corners(Layer::Fore, (0.5, 0.5, total_w - 0.5, h - 0.5), 14.0 * s, (1.5 * s).max(1.0), hud_c(150));

    // Kopfzeile mit Haarlinien links und rechts, kleine Endmarken
    let hw = text::width(fonts, &head, s);
    centered(&mut c, &head, y_head);
    let mid = y_head + 6.0 * s;
    let (x0, x1) = (f, f + w);
    c.line(Layer::Fore, (x0, mid), (cx - hw / 2.0 - 14.0 * s, mid), 1.0, HAIRLINE);
    c.line(Layer::Fore, (cx + hw / 2.0 + 14.0 * s, mid), (x1, mid), 1.0, HAIRLINE);
    for x in [x0, x1] {
        c.line(Layer::Fore, (x, mid - 4.0 * s), (x, mid + 4.0 * s), 1.0, hud_c(170));
    }
    for d in [-1.0, 1.0] {
        let dx = cx + d * (hw / 2.0 + 7.0 * s);
        c.disc(Layer::Fore, (dx, mid), 1.8 * s, hud_c(220));
    }

    // Ziffern: Stunden:Minuten groß, Sekunden kleiner in Cyan auf derselben Grundlinie
    let bx = (cx - digits_w / 2.0).round();
    c.halo(fonts, &big, s, bx, y_big, 14.0, 0.55, hud_c(255));
    c.text(Layer::Fore, fonts, &big, s, bx, y_big);
    if let (Some(l), Some(lay)) = (&sec, &sec_l) {
        let sx = bx + big_l.width + gap;
        let sy = y_big + big_l.ascent - lay.ascent;
        c.halo(fonts, l, s, sx, sy, 10.0, 0.6, hud_c(255));
        c.text(Layer::Fore, fonts, l, s, sx, sy);
    }

    // 24-Stunden-Leiste: Stundenstriche, Fortschritt bis jetzt, Marke
    c.line(Layer::Fore, (x0, y_track), (x1, y_track), 1.0, [255, 255, 255, 70]);
    for i in 0..=24 {
        let tx = x0 + w * i as f32 / 24.0;
        let len = if i % 6 == 0 { 7.0 } else { 3.0 } * s;
        c.line(Layer::Fore, (tx, y_track - len), (tx, y_track), 1.0, [255, 255, 255, if i % 6 == 0 { 150 } else { 90 }]);
    }
    let now_x = x0 + w * k.day.clamp(0.0, 1.0);
    c.line(Layer::Fore, (x0, y_track), (now_x, y_track), 2.0 * s, hud_c(200));
    c.triangle(Layer::Fore, [(now_x - 5.0 * s, y_track - 10.0 * s), (now_x + 5.0 * s, y_track - 10.0 * s), (now_x, y_track - 3.0 * s)], CYAN);
    c.disc(Layer::Fore, (now_x, y_track), 2.6 * s, WHITE);
    for (i, label) in ["00", "06", "12", "18", "24"].into_iter().enumerate() {
        let l = hud::tiny(label, DIM);
        let lw = text::width(fonts, &l, s);
        let tx = x0 + w * i as f32 / 4.0;
        c.text(Layer::Fore, fonts, &l, s, (tx - lw / 2.0).clamp(x0, x1 - lw), y_track + 6.0 * s);
    }

    if let (Some(l), Some(y)) = (&date, y_date) {
        centered(&mut c, l, y);
    }
    if let (Some(l), Some(y)) = (&tele, y_tele) {
        centered(&mut c, l, y);
    }
    // Die Zeichenfläche hat schon den Rand der Ecken eingerechnet; Koordinaten oben sind absolut.
    c.finish(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_weeks() {
        assert_eq!(iso_week(DateTime::new(2026, 11, 11, 0, 0, 0)), 46);
        assert_eq!(iso_week(DateTime::new(2021, 1, 3, 0, 0, 0)), 53); // So 3.1.2021 gehört zu KW 53/2020
        assert_eq!(iso_week(DateTime::new(2024, 12, 30, 0, 0, 0)), 1); // Mo 30.12.2024 = KW 1/2025
        assert_eq!(iso_week(DateTime::new(2026, 1, 1, 0, 0, 0)), 1);
    }

    #[test]
    fn content_and_render() {
        let now = 1_794_429_531; // Mi 11.11.2026, 20:38:51 UTC
        let local = DateTime::from_unix(now + 3600);
        let mut cfg = ClockWidget { enabled: true, ..Default::default() };
        cfg.seconds = true;
        cfg.date = true;
        let k = content(&cfg, local, now);
        assert_eq!(k.hm, "21:38");
        assert_eq!(k.sec.as_deref(), Some(":51"));
        assert_eq!(k.zone, "LOCAL TIME · UTC+01:00");
        assert_eq!(k.telemetry.as_deref(), Some("UTC 20:38:51 · TAG 315 / 365 · KW 46 · Q4"));
        let fonts = Fonts::new();
        let img = render(&k, &fonts, 1.0);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        // Ohne Sekunden und Datum: kleiner, minutengenau
        cfg.seconds = false;
        cfg.date = false;
        let small = content(&cfg, local, now);
        assert!(small.sec.is_none() && small.date.is_none());
        assert_eq!(small, content(&cfg, DateTime::from_unix(now + 3600 + 5), now + 5));
        assert!(render(&small, &fonts, 1.0).height < img.height);
    }
}
