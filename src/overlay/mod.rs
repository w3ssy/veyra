//! Widgets über dem Video: Uhr und Flight Control (nächster Raketenstart).
//!
//! Der Inhalt wird höchstens einmal pro Sekunde neu bestimmt; gerastert und hochgeladen wird nur,
//! wenn er sich tatsächlich ändert (die Uhr ohne Sekunden also einmal pro Minute). Pausierte
//! Monitore kosten nichts – ihre Widgets werden erst beim Fortsetzen aktualisiert.

pub mod launch;
pub mod text;
pub mod time;

use crate::config::{self, Position};
use crate::platform::sys;
use launch::{Feed, View};
use text::{Align, Color, Fonts, Image, Line, Weight, WHITE};
use time::{DateTime, MONTHS, WEEKDAYS, WEEKDAYS_SHORT};

const CYAN: Color = [0x22, 0xD3, 0xEE, 255];
const GREEN: Color = [0x34, 0xD3, 0x99, 255];
const AMBER: Color = [0xF5, 0x9E, 0x0B, 255];
const RED: Color = [0xF8, 0x71, 0x71, 255];
const DIM: Color = [255, 255, 255, 170];
const SOFT: Color = [255, 255, 255, 225];

/// Abstand zum Rand des Arbeitsbereichs bei Skalierung 1.
const MARGIN: f32 = 48.0;
/// Abstand zwischen zwei Widgets an derselben Position.
const STACK_GAP: f32 = 28.0;

/// Inhalt einer Position: die Zeilen aller Widgets, die dort stehen.
pub type Content = Vec<(Position, Vec<Line>)>;

pub struct Widgets {
    fonts: Fonts,
    feed: Option<Feed>,
}

impl Widgets {
    pub fn new() -> Self {
        Self { fonts: Fonts::new(), feed: None }
    }

    /// Datenabruf passend zur Konfiguration starten oder beenden.
    pub fn configure(&mut self, cfg: &config::Widgets) {
        match (cfg.flight_control.enabled, self.feed.is_some()) {
            (true, false) => {
                log::info!("Flight Control aktiv – lade Startdaten von ll.thespacedevs.com");
                self.feed = Some(Feed::start(config::config_dir().join("launches.json")));
            }
            (false, true) => {
                log::info!("Flight Control aus – kein Netzwerkzugriff mehr");
                self.feed = None;
            }
            _ => {}
        }
    }

    /// Was jetzt angezeigt werden soll, gruppiert nach Position (Reihenfolge stabil).
    pub fn content(&self, cfg: &config::Widgets, now: i64) -> Content {
        let mut out: Content = Vec::new();
        let mut add = |pos: Position, lines: Vec<Line>| match out.iter_mut().find(|(p, _)| *p == pos) {
            Some((_, existing)) => {
                let mut lines = lines;
                if let Some(first) = lines.first_mut() {
                    first.gap += STACK_GAP;
                }
                existing.extend(lines);
            }
            None => out.push((pos, lines)),
        };
        if cfg.clock.enabled {
            add(cfg.clock.position, clock(&cfg.clock, sys::local_time(now)));
        }
        if cfg.flight_control.enabled {
            let view = self.feed.as_ref().map_or(View::Waiting, |f| f.view(now));
            add(cfg.flight_control.position, flight_control(view, now));
        }
        out
    }

    pub fn rasterize(&self, lines: &[Line], pos: Position, scale: f32) -> Image {
        let align = match pos.grid().0 {
            0 => Align::Left,
            1 => Align::Center,
            _ => Align::Right,
        };
        text::render(&self.fonts, lines, align, scale)
    }
}

/// Skalierung für einen Monitor: 1.0 bei 1080 Pixel Höhe.
pub fn scale_for(height: u32, cfg: &config::Widgets) -> f32 {
    (height as f32 / 1080.0).max(0.5) * cfg.scale
}

/// Linke obere Ecke des Bildes auf dem Monitor. `insets` = Taskleiste usw. (links, oben, rechts, unten).
pub fn place(pos: Position, img: &Image, surface: (u32, u32), insets: [i32; 4], scale: f32) -> (i32, i32) {
    let margin = (MARGIN * scale).round() as i32;
    let pad = img.pad as i32;
    let axis = |slot: u8, size: u32, before: i32, after: i32, img_size: u32| -> i32 {
        let (size, img_size) = (size as i32, img_size as i32);
        match slot {
            0 => before + margin - pad,
            1 => before + (size - before - after - img_size) / 2,
            _ => size - after - margin - img_size + pad,
        }
    };
    let (col, row) = pos.grid();
    (axis(col, surface.0, insets[0], insets[2], img.width), axis(row, surface.1, insets[1], insets[3], img.height))
}

fn two(n: u32) -> String {
    format!("{n:02}")
}

fn clock(cfg: &config::ClockWidget, t: DateTime) -> Vec<Line> {
    let mut time = format!("{}:{}", two(t.hour), two(t.minute));
    if cfg.seconds {
        time += &format!(":{}", two(t.second));
    }
    let mut lines = vec![Line::new(96.0, Weight::Bold).span(time, WHITE).tabular().tracking(0.03)];
    if cfg.date {
        let date = format!("{}, {}. {} {}", WEEKDAYS[t.weekday as usize], t.day, MONTHS[t.month as usize - 1], t.year);
        lines.push(Line::new(17.0, Weight::Medium).span(date, SOFT).tracking(0.22).gap(8.0));
    }
    lines
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
fn countdown(net: i64, now: i64) -> String {
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

/// Lange Namen kürzen, damit das Widget nicht über den halben Bildschirm reicht.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max - 1).collect::<String>().trim_end().to_string() + "…"
    }
}

fn flight_control(view: View, now: i64) -> Vec<Line> {
    let header = |status: Option<(&str, Color)>, live: bool| {
        let mut l = Line::new(14.0, Weight::Bold).tracking(0.3).span("NEXT LAUNCH", CYAN);
        if let Some((s, c)) = status {
            l = l.span(" / ", DIM).span(s.to_uppercase(), c);
        }
        if live {
            l = l.span(" / ", DIM).span("LIVE", RED);
        }
        l
    };
    let big = |s: String| Line::new(52.0, Weight::Bold).span(s, WHITE).tabular().tracking(0.03).gap(6.0);
    let small = |s: String, c: Color| Line::new(14.0, Weight::Medium).span(s, c).tracking(0.16).gap(8.0);

    let l = match view {
        View::Waiting => return vec![header(None, false), big("T- --:--:--".into()), small("AWAITING TELEMETRY".into(), DIM)],
        View::NoSignal => return vec![header(None, false), big("T- --:--:--".into()), small("NO SIGNAL".into(), RED)],
        View::Launch(l) => l,
    };
    let local = sys::local_time(l.net);
    let when = format!("{} {}.{}. {}:{}", WEEKDAYS_SHORT[local.weekday as usize], two(local.day), two(local.month), two(local.hour), two(local.minute));
    let main = if l.precise || l.net <= now {
        countdown(l.net, now)
    } else {
        format!("NET {}. {}", local.day, MONTHS[local.month as usize - 1].chars().take(3).collect::<String>())
    };
    let mut mission = format!("{} / {}", l.rocket, clip(&l.mission, 34)).to_uppercase();
    if !l.orbit.is_empty() {
        mission += &format!(" / {}", l.orbit.to_uppercase());
    }
    let mut site: Vec<String> = [l.pad.as_str(), l.location.as_str()].into_iter().filter(|s| !s.is_empty()).map(|s| clip(s, 32)).collect();
    if l.precise {
        site.push(when);
    }
    vec![
        header(Some((&l.status, status_color(&l.status))), l.webcast),
        big(main),
        Line::new(19.0, Weight::Bold).span(mission, SOFT).tracking(0.06).gap(6.0),
        small(site.join(" / ").to_uppercase(), DIM),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_format() {
        assert_eq!(countdown(1000 + 2 * 86_400 + 3723, 1000), "T- 2D 01:02:03");
        assert_eq!(countdown(1000 + 59, 1000), "T- 00:00:59");
        assert_eq!(countdown(1000, 1000 + 192), "T+ 00:03:12");
    }

    #[test]
    fn placement_respects_taskbar() {
        let img = Image { width: 300, height: 120, pad: 14, rgba: Vec::new() };
        // Taskleiste unten, 48 px hoch
        let (x, y) = place(Position::BottomLeft, &img, (1920, 1080), [0, 0, 0, 48], 1.0);
        assert_eq!(x, 48 - 14);
        assert_eq!(y + 120 - 14, 1080 - 48 - 48, "Unterkante des Inhalts 48 px über der Taskleiste");
        let (x, _) = place(Position::Center, &img, (1920, 1080), [0, 0, 0, 48], 1.0);
        assert_eq!(x, (1920 - 300) / 2);
    }

    #[test]
    fn stacked_widgets_share_a_position() {
        let w = Widgets::new();
        let mut cfg = config::Widgets::default();
        cfg.clock.enabled = true;
        cfg.flight_control.enabled = true;
        cfg.clock.position = Position::BottomLeft;
        cfg.flight_control.position = Position::BottomLeft;
        let c = w.content(&cfg, time::now_unix());
        assert_eq!(c.len(), 1);
        assert!(c[0].1.len() >= 5);
        cfg.clock.position = Position::Center;
        assert_eq!(w.content(&cfg, time::now_unix()).len(), 2);
    }
}
