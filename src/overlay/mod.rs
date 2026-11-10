//! Widgets über dem Video: Uhr und Flight Control (nächster Raketenstart als Mission-Control-Modul).
//!
//! Der Inhalt wird höchstens einmal pro Sekunde neu bestimmt; gerastert und hochgeladen wird nur,
//! wenn er sich tatsächlich ändert (die Uhr ohne Sekunden also einmal pro Minute). Pausierte
//! Monitore kosten nichts – ihre Widgets werden erst beim Fortsetzen aktualisiert.

pub mod canvas;
pub mod flight;
pub mod launch;
pub mod text;
pub mod time;

use crate::config::{self, Position};
use crate::platform::sys;
use canvas::Image;
use flight::Fx;
use launch::{Feed, View};
use text::{Align, Color, Fonts, Line, Weight, WHITE};
use time::{DateTime, MONTHS, WEEKDAYS};

const SOFT: Color = [255, 255, 255, 225];

/// Abstand zum Rand des Arbeitsbereichs bei Skalierung 1.
const MARGIN: f32 = 48.0;
/// Abstand zwischen zwei Widgets an derselben Position.
const STACK_GAP: f32 = 28.0;

/// Ein Widget mit allem, was es gerade zeigt.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Clock(Vec<Line>),
    Flight(Box<flight::Flight>),
}

impl Widget {
    pub fn fx(&self) -> Fx {
        match self {
            Widget::Flight(f) => f.fx,
            Widget::Clock(_) => Fx::None,
        }
    }
}

/// Inhalt je Position (Reihenfolge stabil).
pub type Content = Vec<(Position, Vec<Widget>)>;

pub use flight::flash;

pub struct Widgets {
    fonts: Fonts,
    feed: Option<Feed>,
    cache: flight::Cache,
}

impl Widgets {
    pub fn new() -> Self {
        Self { fonts: Fonts::new(), feed: None, cache: flight::Cache::default() }
    }

    /// Datenabruf passend zur Konfiguration starten oder beenden.
    pub fn configure(&mut self, cfg: &config::Widgets) {
        match (cfg.flight_control.enabled, self.feed.is_some()) {
            (true, false) => {
                log::info!("Flight Control aktiv – lade Startdaten von ll.thespacedevs.com");
                self.feed = Some(Feed::start(config::config_dir()));
            }
            (false, true) => {
                log::info!("Flight Control aus – kein Netzwerkzugriff mehr");
                self.feed = None;
            }
            _ => {}
        }
    }

    /// Was jetzt angezeigt werden soll, gruppiert nach Position.
    pub fn content(&self, cfg: &config::Widgets, now: i64) -> Content {
        let mut out: Content = Vec::new();
        let mut add = |pos: Position, w: Widget| match out.iter_mut().find(|(p, _)| *p == pos) {
            Some((_, list)) => list.push(w),
            None => out.push((pos, vec![w])),
        };
        if cfg.clock.enabled {
            add(cfg.clock.position, Widget::Clock(clock(&cfg.clock, sys::local_time(now))));
        }
        if cfg.flight_control.enabled {
            let view = self.feed.as_ref().map_or(View::Waiting, |f| f.view(now));
            add(cfg.flight_control.position, Widget::Flight(Box::new(flight::content(view, now))));
        }
        out
    }

    /// Alle Widgets einer Position rastern und untereinander stapeln.
    pub fn rasterize(&mut self, widgets: &[Widget], pos: Position, scale: f32) -> Image {
        let align = match pos.grid().0 {
            0 => Align::Left,
            1 => Align::Center,
            _ => Align::Right,
        };
        let images: Vec<Image> = widgets
            .iter()
            .map(|w| match w {
                Widget::Clock(lines) => text::render(&self.fonts, lines, align, scale),
                Widget::Flight(f) => flight::render(f, &self.fonts, scale, &mut self.cache),
            })
            .collect();
        stack(images, align, (STACK_GAP * scale).round() as u32)
    }
}

/// Bilder untereinander setzen (alle mit gleichem Schattenrand, da gleiche Skalierung).
fn stack(mut images: Vec<Image>, align: Align, gap: u32) -> Image {
    if images.len() == 1 {
        return images.pop().unwrap();
    }
    let pad = images.iter().map(|i| i.pad).max().unwrap_or(0);
    let width = images.iter().map(|i| i.width).max().unwrap_or(1);
    let inner: u32 = images.iter().map(|i| i.height - 2 * i.pad).sum::<u32>() + gap * (images.len() as u32 - 1);
    let height = inner + 2 * pad;
    let with_glow = images.iter().any(|i| !i.glow.is_empty());
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    let mut glow = if with_glow { vec![0u8; rgba.len()] } else { Vec::new() };
    let mut y = 0u32;
    for img in &images {
        let x = match align {
            Align::Left => 0,
            Align::Center => (width - img.width) / 2,
            Align::Right => width - img.width,
        };
        // Pixel "über" das bisherige legen – die Schattenränder benachbarter Bilder dürfen sich überlappen.
        let over = |dst: &mut [u8], src: &[u8]| {
            for row in 0..img.height {
                for col in 0..img.width {
                    let s = ((row * img.width + col) * 4) as usize;
                    let d = (((y + row) * width + x + col) * 4) as usize;
                    let inv = 255 - src[s + 3] as u32;
                    for k in 0..4 {
                        dst[d + k] = (src[s + k] as u32 + dst[d + k] as u32 * inv / 255).min(255) as u8;
                    }
                }
            }
        };
        over(&mut rgba, &img.rgba);
        if !img.glow.is_empty() {
            over(&mut glow, &img.glow);
        }
        y += img.height - 2 * img.pad + gap;
    }
    Image { width, height, pad, rgba, glow }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_respects_taskbar() {
        let img = Image { width: 300, height: 120, pad: 14, rgba: Vec::new(), glow: Vec::new() };
        // Taskleiste unten, 48 px hoch
        let (x, y) = place(Position::BottomLeft, &img, (1920, 1080), [0, 0, 0, 48], 1.0);
        assert_eq!(x, 48 - 14);
        assert_eq!(y + 120 - 14, 1080 - 48 - 48, "Unterkante des Inhalts 48 px über der Taskleiste");
        let (x, _) = place(Position::Center, &img, (1920, 1080), [0, 0, 0, 48], 1.0);
        assert_eq!(x, (1920 - 300) / 2);
    }

    #[test]
    fn stacked_widgets_share_a_position() {
        let mut w = Widgets::new();
        let mut cfg = config::Widgets::default();
        cfg.clock.enabled = true;
        cfg.flight_control.enabled = true;
        cfg.clock.position = Position::BottomLeft;
        cfg.flight_control.position = Position::BottomLeft;
        let c = w.content(&cfg, time::now_unix());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1.len(), 2);
        let both = w.rasterize(&c[0].1, Position::BottomLeft, 1.0);
        let single = w.rasterize(&c[0].1[1..], Position::BottomLeft, 1.0);
        assert!(both.height > single.height + 80);
        assert_eq!(both.rgba.len(), (both.width * both.height * 4) as usize);
        cfg.clock.position = Position::Center;
        assert_eq!(w.content(&cfg, time::now_unix()).len(), 2);
    }
}


