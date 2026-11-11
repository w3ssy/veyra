//! Vorschaubilder der Widgets als PNG – nur für die Entwicklung:
//! `cargo test preview -- --ignored` schreibt nach `target/preview/`.

use super::canvas::Image;
use super::text::Fonts;
use super::launch::{Launch, LaunchView, View};
use super::{analyzer, dsn, dsnnow, flight, place, spaceclock, Widget};
use crate::config::Position;
use crate::audio::scope::{Scope, BANDS};
use crate::config::{AnalyzerMode, DeepSpaceClockWidget};
use std::path::Path;
use std::time::{Duration, Instant};

/// Unkomprimiertes PNG (Deflate „stored“) – reicht für Vorschauen und braucht keine Bibliothek.
fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) {
    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for row in rgb.chunks_exact((w * 3) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &v in &raw {
        a = (a + v as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}

/// Bild über einen dunklen „Weltraum“-Verlauf legen, Leuchten addieren (wie der Shader).
fn compose(img: &Image, glow: Option<(&analyzer::Screen, &[u8])>) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let t = x as f32 / w as f32;
            let bg = [18.0 + 40.0 * t, 14.0 + 10.0 * (y as f32 / h as f32), 46.0 + 30.0 * (1.0 - t)];
            let p = &img.rgba[i * 4..i * 4 + 4];
            let a = p[3] as f32 / 255.0;
            let mut g = [0.0f32; 3];
            if !img.glow.is_empty() {
                g = [0, 1, 2].map(|k| img.glow[i * 4 + k] as f32);
            }
            if let Some(((sx, sy, sw, sh), data)) = glow {
                let (lx, ly) = (x as i64 - *sx as i64, y as i64 - *sy as i64);
                if lx >= 0 && ly >= 0 && lx < *sw as i64 && ly < *sh as i64 {
                    let j = (ly as usize * *sw as usize + lx as usize) * 4;
                    g = [0, 1, 2].map(|k| data[j + k] as f32);
                }
            }
            for k in 0..3 {
                out[i * 3 + k] = (p[k] as f32 + bg[k] * (1.0 - a) + g[k]).min(255.0) as u8;
            }
        }
    }
    out
}

/// Etwas Musik-Ähnliches: Bass, Akkord, Rauschen, leicht verschiedene Kanäle.
fn music(frame: usize) -> Scope {
    let mut s = Scope { active: true, rate: 48_000, peak_db: -6.4, ..Default::default() };
    let mut seed = 0x1234_5678u32 ^ frame as u32;
    let mut noise = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32 - 0.5) * 0.05
    };
    let off = frame * 800;
    for i in 0..s.left.len() {
        let t = (off + i) as f32 / 48_000.0;
        let tau = std::f32::consts::TAU;
        let bass = 0.35 * (tau * 55.0 * t).sin();
        let chord = 0.12 * (tau * 220.0 * t).sin() + 0.09 * (tau * 277.2 * t + 0.4).sin() + 0.07 * (tau * 329.6 * t).sin();
        let n = noise();
        s.left[i] = bass + chord + n;
        s.right[i] = bass + chord * 0.7 + 0.08 * (tau * 440.0 * t).sin() - n;
    }
    s.bands = (0..BANDS)
        .map(|b| {
            let x = b as f32 / BANDS as f32;
            (0.95 - 0.55 * x + 0.18 * (x * 19.0 + frame as f32 * 0.3).sin()).clamp(0.05, 1.0) * if b < 4 { 0.8 } else { 1.0 }
        })
        .collect();
    s.peaks = s.bands.iter().map(|v| (v + 0.07).min(1.0)).collect();
    s
}

#[test]
#[ignore]
fn preview() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("preview");
    std::fs::create_dir_all(&dir).unwrap();
    let fonts = Fonts::new();
    let now = super::time::now_unix();

    let cfg = DeepSpaceClockWidget { longitude: Some(13.40), ..Default::default() };
    let img = spaceclock::render(&spaceclock::content(&cfg, now), &fonts, 1.0, spaceclock::WIDTH);
    write_png(&dir.join("deep_space_clock.png"), img.width, img.height, &compose(&img, None));

    for mode in [AnalyzerMode::Spectrum, AnalyzerMode::Wave, AnalyzerMode::Xy] {
        let (img, screen) = analyzer::render(&analyzer::content(mode, &music(0)), &fonts, 1.0, analyzer::WIDTH);
        let mut ph = analyzer::Phosphor::new(screen, mode, 1.0);
        let mut t = Instant::now();
        for f in 0..14 {
            t += Duration::from_millis(16);
            ph.step(&music(f), t);
        }
        let name = format!("signal_analyzer_{}.png", mode.as_str());
        write_png(&dir.join(name), img.width, img.height, &compose(&img, Some((&screen, ph.current()))));
    }

    let xml = r#"<dsn>
<station name="gdscc" friendlyName="Goldstone"/>
<dish name="DSS14"><downSignal signalType="data" dataRate="160" spacecraft="VGR2"/><target name="VGR2" downlegRange="2.13e10" rtlt="142000"/></dish>
<dish name="DSS26"><upSignal signalType="data" dataRate="2000" spacecraft="M20"/><downSignal signalType="data" dataRate="2.2e6" spacecraft="M20"/><target name="M20" downlegRange="3.1e8" rtlt="2068"/></dish>
<dish name="DSS24"><downSignal signalType="carrier" spacecraft="PSYC"/><target name="PSYC" downlegRange="4.6e8" rtlt="3069"/></dish>
<station name="mdscc" friendlyName="Madrid"/>
<dish name="DSS63"><downSignal signalType="data" dataRate="2.8e7" spacecraft="JWST"/><upSignal signalType="data" dataRate="16000" spacecraft="JWST"/><target name="JWST" downlegRange="1.5e6" rtlt="10.0"/></dish>
<dish name="DSS56"><downSignal signalType="data" dataRate="40000" spacecraft="EURC"/><target name="EURC" downlegRange="6.4e8" rtlt="4270"/></dish>
<station name="cdscc" friendlyName="Canberra"/>
<dish name="DSS35" activity="Engineering Upgrades"><target name="DSN" rtlt="-1"/></dish>
<timestamp>1794450905000</timestamp></dsn>"#;
    let snap = std::sync::Arc::new(dsn::parse(xml).unwrap());
    let dsn_now = dsnnow::content(dsn::View::Live(snap.clone()), snap.time + 20);
    let img = dsnnow::render(&dsn_now, &fonts, 1.0, dsnnow::WIDTH);
    write_png(&dir.join("dsn_now.png"), img.width, img.height, &compose(&img, None));

    let clock_cfg = crate::config::ClockWidget { enabled: true, seconds: true, date: true, ..Default::default() };
    let clock = super::clockface::content(&clock_cfg, crate::platform::sys::local_time(now), now);
    let img = super::clockface::render(&clock, &fonts, 1.0);
    write_png(&dir.join("clock.png"), img.width, img.height, &compose(&img, None));

    let tle = super::orbit::tests::iss();
    let iss_now = super::iss::content(super::iss::View::Live(tle.clone()), tle.epoch as i64 + 2400);
    // Mit Radar-Puls mitten im Lauf, wie er live aussieht.
    for (name, at, phase) in [("iss_tracker.png", 2400, 0.35), ("iss_tracker_2.png", 4300, 0.6)] {
        let d = super::iss::content(super::iss::View::Live(tle.clone()), tle.epoch as i64 + at);
        let (img, spot) = super::iss::render(&d, &fonts, 1.0, super::iss::WIDTH);
        let spot = spot.unwrap();
        let mut radar = super::iss::Radar::new(spot, 1.0);
        radar.draw(Some(phase));
        write_png(&dir.join(name), img.width, img.height, &compose(&img, Some((&spot.rect, radar.current()))));
    }

    let mut cache = flight::Cache::default();
    let fl = flight::content(rocket(now + 21_447), now);
    let img = flight::render(&fl, &fonts, 1.0, &mut cache, flight::WIDTH);
    write_png(&dir.join("flight_control.png"), img.width, img.height, &compose(&img, None));

    // Gesamtansicht 1920 × 1080 mit Taskleiste: wie im Alltag verteilt, und einmal gestapelt.
    let dsc = Widget::SpaceClock(Box::new(spaceclock::content(&cfg, now)));
    let ana = Widget::Analyzer(analyzer::content(AnalyzerMode::Spectrum, &music(0)));
    let layouts: [(&str, super::Content); 2] = [
        (
            "dashboard.png",
            vec![
                (Position::TopLeft, vec![Widget::Dsn(Box::new(dsn_now.clone()))]),
                (Position::TopRight, vec![dsc.clone()]),
                (Position::BottomLeft, vec![Widget::Flight(Box::new(fl.clone()))]),
                (Position::BottomRight, vec![ana.clone()]),
                (Position::Center, vec![Widget::Clock(Box::new(clock.clone()))]),
                (Position::Bottom, vec![Widget::Iss(Box::new(iss_now.clone()))]),
            ],
        ),
        (
            "dashboard_stacked.png",
            vec![
                (Position::TopLeft, vec![Widget::Dsn(Box::new(dsn_now)), Widget::Flight(Box::new(fl))]),
                (Position::TopRight, vec![dsc, ana]),
            ],
        ),
    ];
    for (name, layout) in layouts {
        let mut widgets = super::Widgets::new();
        let (sw, sh) = (1920u32, 1080u32);
        let mut out: Vec<f32> = (0..sw * sh)
            .flat_map(|i| {
                let (x, y) = ((i % sw) as f32 / sw as f32, (i / sw) as f32 / sh as f32);
                // Heller Nebel in der Mitte oben, sonst dunkler Weltraum – prüft die Lesbarkeit.
                let d = ((x - 0.62).powi(2) + (y - 0.25).powi(2) * 2.0).sqrt();
                let neb = (1.0 - d * 2.4).max(0.0).powf(1.5);
                [20.0 + 215.0 * neb, 24.0 + 190.0 * neb, 34.0 + 150.0 * neb]
            })
            .collect();
        for (pos, list) in layout {
            let (img, live) = widgets.rasterize(&list, pos, 1.0);
            let (px, py) = place(pos, &img, (sw, sh), [0, 0, 0, 48], 1.0);
            let glows: Vec<(analyzer::Screen, Vec<u8>)> = live
                .into_iter()
                .map(|l| match l {
                    super::LiveScreen::Analyzer { rect, mode } => {
                        let mut ph = analyzer::Phosphor::new(rect, mode, 1.0);
                        let mut t = Instant::now();
                        for f in 0..14 {
                            t += Duration::from_millis(16);
                            ph.step(&music(f), t);
                        }
                        (rect, ph.current().to_vec())
                    }
                    super::LiveScreen::Radar(spot) => {
                        let mut r = super::iss::Radar::new(spot, 1.0);
                        r.draw(Some(0.35));
                        (spot.rect, r.current().to_vec())
                    }
                })
                .collect();
            for y in 0..img.height {
                for x in 0..img.width {
                    let (ox, oy) = (px + x as i32, py + y as i32);
                    if ox < 0 || oy < 0 || ox >= sw as i32 || oy >= sh as i32 {
                        continue;
                    }
                    let i = ((y * img.width + x) * 4) as usize;
                    let o = (oy as u32 * sw + ox as u32) as usize * 3;
                    let a = img.rgba[i + 3] as f32 / 255.0;
                    for k in 0..3 {
                        let mut g = if img.glow.is_empty() { 0.0 } else { img.glow[i + k] as f32 };
                        for ((gx, gy, gw, gh), data) in &glows {
                            if x >= *gx && y >= *gy && x < gx + gw && y < gy + gh {
                                g = data[(((y - gy) * gw + x - gx) * 4) as usize + k] as f32;
                            }
                        }
                        out[o + k] = img.rgba[i + k] as f32 + out[o + k] * (1.0 - a) + g;
                    }
                }
            }
        }
        let rgb: Vec<u8> = out.iter().map(|v| v.min(255.0) as u8).collect();
        write_png(&dir.join(name), sw, sh, &rgb);
    }
    println!("Vorschau in {}", dir.display());
}

fn rocket(net: i64) -> View {
    View::Launch(Box::new(LaunchView {
        launch: Launch {
            id: "x".into(),
            rocket: "Falcon 9".into(),
            mission: "USSF-627".into(),
            orbit: "LEO".into(),
            net,
            precise: true,
            exact: true,
            status: "Go".into(),
            pad: "SLC-4E".into(),
            location: "Vandenberg SFB".into(),
            webcast: false,
            image: String::new(),
        },
        photo: None,
        patch: None,
    }))
}
