//! Widgets über dem Video: Uhr, Flight Control (nächster Raketenstart als Mission-Control-Modul),
//! Deep Space Clock, Signal Analyzer und DSN Now.
//!
//! Der Inhalt wird höchstens einmal pro Sekunde neu bestimmt; gerastert und hochgeladen wird nur,
//! wenn er sich tatsächlich ändert (die Uhr ohne Sekunden also einmal pro Minute). Pausierte
//! Monitore kosten nichts – ihre Widgets werden erst beim Fortsetzen aktualisiert.
//! Einzige Ausnahme vom Sekundentakt ist die Leuchtspur des Signal Analyzers (siehe [`analyzer`]).

pub mod analyzer;
pub mod astro;
pub mod canvas;
pub mod clockface;
pub mod dsn;
pub mod dsnnow;
pub mod flight;
pub mod hud;
pub mod launch;
#[cfg(test)]
mod preview;
pub mod spaceclock;
pub mod text;
pub mod time;

use crate::audio::SharedScope;
use crate::config::{self, AnalyzerMode, Position};
use crate::platform::sys;
use canvas::Image;
use flight::Fx;
use launch::{Feed, View};
use text::{Align, Fonts};

/// Abstand zum Rand des Arbeitsbereichs bei Skalierung 1.
const MARGIN: f32 = 28.0;
/// Abstand zwischen zwei Widgets an derselben Position – Panels rücken wie Kacheln zusammen.
const STACK_GAP: f32 = 10.0;

/// Ein Widget mit allem, was es gerade zeigt.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    Clock(Box<clockface::Clock>),
    Flight(Box<flight::Flight>),
    SpaceClock(Box<spaceclock::SpaceClock>),
    Analyzer(analyzer::Analyzer),
    Dsn(Box<dsnnow::DsnNow>),
}

impl Widget {
    /// Mindestbreite des Panels (Außenmaß bei Skalierung 1); die Uhr ist freier Text ohne Panel.
    fn panel_width(&self) -> Option<f32> {
        match self {
            Widget::Clock(_) => None,
            Widget::Flight(_) => Some(flight::WIDTH),
            Widget::SpaceClock(_) => Some(spaceclock::WIDTH),
            Widget::Analyzer(_) => Some(analyzer::WIDTH),
            Widget::Dsn(_) => Some(dsnnow::WIDTH),
        }
    }

    pub fn fx(&self) -> Fx {
        match self {
            Widget::Flight(f) => f.fx,
            _ => Fx::None,
        }
    }
}

/// Schirm des Signal Analyzers innerhalb eines gerasterten Panels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LiveScreen {
    pub rect: analyzer::Screen,
    pub mode: AnalyzerMode,
}

/// Inhalt je Position (Reihenfolge stabil).
pub type Content = Vec<(Position, Vec<Widget>)>;

pub use flight::flash;

pub struct Widgets {
    fonts: Fonts,
    feed: Option<Feed>,
    dsn: Option<dsn::Feed>,
    cache: flight::Cache,
    /// Messdaten aus dem Audio-Thread (für den Signal Analyzer).
    scope: SharedScope,
}

impl Widgets {
    pub fn new() -> Self {
        Self { fonts: Fonts::new(), feed: None, dsn: None, cache: flight::Cache::default(), scope: Default::default() }
    }

    /// Wohin der Audio-Thread die Messdaten schreibt.
    pub fn scope(&self) -> SharedScope {
        self.scope.clone()
    }

    /// Datenabruf passend zur Konfiguration starten oder beenden.
    pub fn configure(&mut self, cfg: &config::Widgets) {
        match (cfg.flight_control.enabled, self.feed.is_some()) {
            (true, false) => {
                log::info!("Flight Control aktiv – lade Startdaten von ll.thespacedevs.com");
                self.feed = Some(Feed::start(config::config_dir()));
            }
            (false, true) => {
                log::info!("Flight Control aus – kein Netzwerkzugriff mehr für Startdaten");
                self.feed = None;
            }
            _ => {}
        }
        match (cfg.dsn.enabled, self.dsn.is_some()) {
            (true, false) => {
                log::info!("DSN Now aktiv – lade Antennenstatus von eyes.nasa.gov");
                self.dsn = Some(dsn::Feed::start());
            }
            (false, true) => {
                log::info!("DSN Now aus – kein Netzwerkzugriff mehr für DSN-Daten");
                self.dsn = None;
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
            add(cfg.clock.position, Widget::Clock(Box::new(clockface::content(&cfg.clock, sys::local_time(now), now))));
        }
        if cfg.flight_control.enabled {
            let view = self.feed.as_ref().map_or(View::Waiting, |f| f.view(now));
            add(cfg.flight_control.position, Widget::Flight(Box::new(flight::content(view, now))));
        }
        if cfg.deep_space_clock.enabled {
            add(cfg.deep_space_clock.position, Widget::SpaceClock(Box::new(spaceclock::content(&cfg.deep_space_clock, now))));
        }
        if cfg.signal_analyzer.enabled {
            if let Ok(s) = self.scope.lock() {
                add(cfg.signal_analyzer.position, Widget::Analyzer(analyzer::content(cfg.signal_analyzer.mode, &s)));
            }
        }
        if cfg.dsn.enabled {
            let view = self.dsn.as_ref().map_or(dsn::View::Waiting, |f| f.view());
            add(cfg.dsn.position, Widget::Dsn(Box::new(dsnnow::content(view, now))));
        }
        out
    }

    /// Alle Widgets einer Position rastern und untereinander stapeln. Liefert dazu, wo im Bild
    /// der Schirm des Signal Analyzers liegt (falls er dabei ist).
    pub fn rasterize(&mut self, widgets: &[Widget], pos: Position, scale: f32) -> (Image, Option<LiveScreen>) {
        let align = match pos.grid().0 {
            0 => Align::Left,
            1 => Align::Center,
            _ => Align::Right,
        };
        // Gestapelte Panels bekommen alle die Breite des breitesten – sie bilden eine bündige Spalte.
        let width = widgets.iter().filter_map(Widget::panel_width).fold(0.0, f32::max);
        let mut live = None;
        let images: Vec<Image> = widgets
            .iter()
            .enumerate()
            .map(|(i, w)| match w {
                Widget::Clock(k) => clockface::render(k, &self.fonts, scale),
                Widget::Flight(f) => flight::render(f, &self.fonts, scale, &mut self.cache, width),
                Widget::SpaceClock(c) => spaceclock::render(c, &self.fonts, scale, width),
                Widget::Dsn(d) => dsnnow::render(d, &self.fonts, scale, width),
                Widget::Analyzer(a) => {
                    let (img, rect) = analyzer::render(a, &self.fonts, scale, width);
                    live = Some((i, LiveScreen { rect, mode: a.mode() }));
                    img
                }
            })
            .collect();
        let (img, offsets) = stack(images, align, (STACK_GAP * scale).round() as u32);
        let live = live.map(|(i, mut l)| {
            l.rect.0 += offsets[i].0;
            l.rect.1 += offsets[i].1;
            l
        });
        (img, live)
    }
}

/// Bilder untereinander setzen (alle mit gleichem Schattenrand, da gleiche Skalierung).
/// Liefert zusätzlich die linke obere Ecke jedes Einzelbildes im Gesamtbild.
fn stack(mut images: Vec<Image>, align: Align, gap: u32) -> (Image, Vec<(u32, u32)>) {
    if images.len() == 1 {
        return (images.pop().unwrap(), vec![(0, 0)]);
    }
    let mut offsets = Vec::with_capacity(images.len());
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
        offsets.push((x, y));
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
    (Image { width, height, pad, rgba, glow }, offsets)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_respects_taskbar() {
        let img = Image { width: 300, height: 120, pad: 14, rgba: Vec::new(), glow: Vec::new() };
        // Taskleiste unten, 48 px hoch
        let (x, y) = place(Position::BottomLeft, &img, (1920, 1080), [0, 0, 0, 48], 1.0);
        assert_eq!(x, MARGIN as i32 - 14);
        assert_eq!(y + 120 - 14, 1080 - 48 - MARGIN as i32, "Unterkante des Inhalts MARGIN über der Taskleiste");
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
        let (both, _) = w.rasterize(&c[0].1, Position::BottomLeft, 1.0);
        let (single, _) = w.rasterize(&c[0].1[1..], Position::BottomLeft, 1.0);
        assert!(both.height > single.height + 80);
        assert_eq!(both.rgba.len(), (both.width * both.height * 4) as usize);
        cfg.clock.position = Position::Center;
        assert_eq!(w.content(&cfg, time::now_unix()).len(), 2);
    }

    #[test]
    fn analyzer_screen_survives_stacking() {
        let mut w = Widgets::new();
        let mut cfg = config::Widgets::default();
        cfg.deep_space_clock.enabled = true;
        cfg.signal_analyzer.enabled = true;
        cfg.deep_space_clock.position = Position::Right;
        cfg.signal_analyzer.position = Position::Right;
        let c = w.content(&cfg, time::now_unix());
        assert_eq!(c.len(), 1);
        let (alone, screen_alone) = w.rasterize(&c[0].1[1..], Position::Right, 1.0);
        let (both, screen) = w.rasterize(&c[0].1, Position::Right, 1.0);
        let (a, b) = (screen_alone.unwrap().rect, screen.unwrap().rect);
        assert_eq!((a.2, a.3), (b.2, b.3));
        // Rechtsbündig gestapelt: der Analyzer liegt unter der Uhr.
        assert_eq!(b.0, a.0 + (both.width - alone.width));
        assert!(b.1 > a.1 + 100);
        assert!(b.0 + b.2 <= both.width && b.1 + b.3 <= both.height);
    }
}


