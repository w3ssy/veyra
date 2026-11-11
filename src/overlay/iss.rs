//! ISS Tracker – die Internationale Raumstation als Minimap wie in einem Spiel:
//!
//! ```text
//!  ISS TRACKER ─────────────────────── ● SUNLIT  ● LIVE
//!  ┌                                                   ┐
//!    · · · ·:::::· · ·          ╭ ─ ─ ╮        · · · N    Karte scrollt mit, die ISS bleibt in der Mitte:
//!    · ·:::::::::::· ·  ─ ─ ─ ─(  ▲  )─ ─ ─ ─ ─ ─ ·       Landmassen als Punktraster, Nachtseite gedimmt,
//!    · · ·::::::· · · ·         ╰ ─ ─ ╯   · · · · ·       Tag-Nacht-Grenze, Bodenspur, Sichtbarkeitsradius
//!   ├─┤ 1000 KM
//!  └                                                   ┘
//!  LAT        LON         ALT       VELOCITY      ORBIT
//!  51.24° N   13.40° E    418 KM    27.580 KM/H   50123
//! ```
//!
//! Netzwerk: alle 6 Stunden die Bahnelemente (TLE) von celestrak.org, zwischengespeichert in `iss.tle`.
//! Die Position rechnet [`super::orbit`] daraus lokal – sie läuft also auch ohne Netz weiter.

use super::canvas::{Canvas, Image, Layer};
use super::flight::{AMBER, CYAN, DIM, GREEN, RED, SOFT};
use super::hud;
use super::orbit::{self, Tle};
use super::text::{self, Color, Fonts, Line, Weight, WHITE};
use crate::platform::sys;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const HOST: &str = "celestrak.org";
const PATH: &str = "/NORAD/elements/gp.php?CATNR=25544&FORMAT=TLE";
const REFRESH: Duration = Duration::from_secs(6 * 3600);
const RETRY: Duration = Duration::from_secs(30 * 60);
const MAX_BYTES: usize = 4096;
/// Älter als das: Bahn wird ungenau (die ISS bremst und wird regelmäßig angehoben).
const OLD_TLE: f64 = 3.0 * 86_400.0;

/// Landmaske 720 × 360 (0,5° je Zelle) aus Natural Earth, erzeugt mit `assets/make_land.py`.
static LAND: &[u8; 32_400] = include_bytes!("../../assets/land-720x360.bin");

fn is_land(lat: f64, lon: f64) -> bool {
    let row = ((90.0 - lat) * 2.0).floor().clamp(0.0, 359.0) as usize;
    let col = ((lon + 180.0) * 2.0).floor().rem_euclid(720.0) as usize;
    LAND[row * 90 + col / 8] >> (7 - col % 8) & 1 == 1
}

/// Längendifferenz auf −180 … 180° bringen.
fn wrap(d: f64) -> f64 {
    (d + 540.0).rem_euclid(360.0) - 180.0
}

// ── Datenabruf ───────────────────────────────────────────────────────────────

#[derive(Default)]
struct State {
    tle: Option<Tle>,
    error: bool,
}

pub enum View {
    Waiting,
    NoSignal,
    Live(Tle),
}

pub struct Feed {
    state: Arc<Mutex<State>>,
    _stop: Sender<()>,
}

impl Feed {
    /// `dir` = Konfigurationsordner (dort liegt der Zwischenspeicher `iss.tle`).
    pub fn start(dir: PathBuf) -> Self {
        let file = dir.join("iss.tle");
        let cached = std::fs::read_to_string(&file).ok().and_then(|t| orbit::parse(&t));
        // Frisch genug? Dann erst später abrufen – CelesTrak bittet, dieselben Daten nicht öfter als nötig zu laden.
        let age = std::fs::metadata(&file).and_then(|m| m.modified()).ok().and_then(|m| SystemTime::now().duration_since(m).ok());
        let mut wait = match (&cached, age) {
            (Some(_), Some(age)) if age < REFRESH => REFRESH - age,
            _ => Duration::ZERO,
        };
        let state = Arc::new(Mutex::new(State { tle: cached, error: false }));
        let (stop, rx) = channel::<()>();
        let shared = state.clone();
        let spawned = std::thread::Builder::new().name("iss-tracker".into()).spawn(move || loop {
            if !matches!(rx.recv_timeout(wait), Err(RecvTimeoutError::Timeout)) {
                log::debug!("ISS Tracker beendet");
                break;
            }
            wait = match fetch() {
                Ok((text, tle)) => {
                    log::debug!("ISS Tracker: neue Bahndaten, Umlauf {}", tle.rev);
                    if let Err(e) = std::fs::write(&file, text) {
                        log::warn!("ISS Tracker: {file:?} schreiben: {e}");
                    }
                    let mut s = shared.lock().unwrap();
                    s.tle = Some(tle);
                    s.error = false;
                    REFRESH
                }
                Err(e) => {
                    log::warn!("ISS Tracker: {e:#}");
                    shared.lock().unwrap().error = true;
                    RETRY
                }
            };
        });
        if let Err(e) = spawned {
            log::error!("ISS Tracker starten: {e}");
        }
        Self { state, _stop: stop }
    }

    pub fn view(&self) -> View {
        let s = self.state.lock().unwrap();
        match &s.tle {
            Some(t) => View::Live(t.clone()),
            None if s.error => View::NoSignal,
            None => View::Waiting,
        }
    }
}

fn fetch() -> anyhow::Result<(String, Tle)> {
    let (status, body) = sys::https_get(HOST, PATH, MAX_BYTES)?;
    anyhow::ensure!(status == 200, "HTTP {status}");
    let text = String::from_utf8(body)?;
    let tle = orbit::parse(&text).ok_or_else(|| anyhow::anyhow!("keine gültigen Bahndaten"))?;
    Ok((text, tle))
}

// ── Inhalt ───────────────────────────────────────────────────────────────────

/// Sichtbarer Kartenausschnitt: so viele Längengrade über die volle Breite.
const SPAN_LON: f64 = 160.0;
/// Bodenspur: so weit zurück und voraus (Sekunden) und in diesem Abstand.
const TRACK_BACK: i64 = 45 * 60;
const TRACK_AHEAD: i64 = 95 * 60;
const TRACK_STEP: i64 = 30;

#[derive(Debug, Clone, PartialEq)]
pub struct Pos {
    lat: f32,
    lon: f32,
    alt_km: f32,
    speed_kmh: u32,
    /// Flugrichtung auf der Karte (Bogenmaß, 0 = Osten, gegen den Uhrzeigersinn).
    heading: f32,
    rev: u32,
    /// Bodenspur (Breite, Länge); die ersten `past` Punkte liegen in der Vergangenheit.
    track: Vec<(f32, f32)>,
    past: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Iss {
    status: (&'static str, Color),
    sunlit: Option<bool>,
    pos: Option<Pos>,
    /// Bildpunkt der Sonne (Breite, Länge).
    sun: (f32, f32),
}

pub fn content(view: View, now: i64) -> Iss {
    let t = now as f64;
    let ((dec, slon), sun_dir) = orbit::sun(t);
    let sun = (dec as f32, slon as f32);
    let tle = match view {
        View::Waiting => return Iss { status: ("ACQUIRING", AMBER), sunlit: None, pos: None, sun },
        View::NoSignal => return Iss { status: ("NO SIGNAL", RED), sunlit: None, pos: None, sun },
        View::Live(tle) => tle,
    };
    let s = tle.at(t);
    let ahead = tle.at(t + 10.0);
    let heading = (ahead.lat - s.lat).atan2(wrap(ahead.lon - s.lon)) as f32;
    let track: Vec<(f32, f32)> = (-TRACK_BACK..=TRACK_AHEAD)
        .step_by(TRACK_STEP as usize)
        .map(|d| {
            let p = tle.at(t + d as f64);
            (p.lat as f32, p.lon as f32)
        })
        .collect();
    let status = if t - tle.epoch > OLD_TLE { ("OLD TLE", AMBER) } else { ("LIVE", GREEN) };
    Iss {
        status,
        sunlit: Some(!orbit::in_shadow(s.eci, sun_dir)),
        pos: Some(Pos {
            lat: s.lat as f32,
            lon: s.lon as f32,
            alt_km: s.alt_km as f32,
            speed_kmh: (s.speed_kms * 3600.0 / 10.0).round() as u32 * 10,
            heading,
            rev: s.rev,
            track,
            past: (TRACK_BACK / TRACK_STEP) as usize,
        }),
        sun,
    }
}

fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(c);
    }
    out
}

// ── Darstellung ──────────────────────────────────────────────────────────────

/// Mindestbreite des Panels (Außenmaß bei Skalierung 1).
pub const WIDTH: f32 = 500.0;
const MAP_Y: f32 = hud::HEAD_H;
const MAP_H: f32 = 196.0;
/// Innenhöhe: Kopf, Karte, Datenzeile.
const H: f32 = MAP_Y + MAP_H + 36.0;
/// Abstand der Kartenpunkte in Grad – fest an der Erde, damit sie beim Scrollen mitwandern.
const DOT: f64 = 1.5;

/// `w` = Außenbreite des Panels (mindestens [`WIDTH`]), bei Skalierung 1.
pub fn render(d: &Iss, fonts: &Fonts, s: f32, w: f32) -> Image {
    let iw = hud::inner(w);
    let mut c = hud::panel(w, H + 2.0 * hud::PAD, s);
    let mut pills: Vec<(&str, Color)> = vec![d.status];
    match d.sunlit {
        Some(true) => pills.push(("SUNLIT", AMBER)),
        Some(false) => pills.push(("ECLIPSE", DIM)),
        None => {}
    }
    hud::header(&mut c, fonts, s, (0.0, iw * s), 0.0, ("ISS TRACKER", CYAN), &pills);

    let (mx0, my0) = (0.0f32, (MAP_Y * s).round());
    let (mw, mh) = ((iw * s).round(), (MAP_H * s).round());
    let (mx1, my1) = (mx0 + mw, my0 + mh);
    let cyan = |a: u8| [CYAN[0], CYAN[1], CYAN[2], a];
    c.rect(Layer::Back, (mx0, my0, mx1, my1), [2, 8, 16, 150]);
    c.set_clip(Some((mx0, my0, mx1, my1)));

    // Projektion: ISS in der Mitte (gleichabständig, Grad pro Pixel überall gleich).
    let k = SPAN_LON / mw as f64;
    let half_h = mh as f64 / 2.0 * k;
    let (clat, clon) = d.pos.as_ref().map_or((20.0, 0.0), |p| (p.lat as f64, p.lon as f64));
    let clat = clat.clamp(-90.0 + half_h, 90.0 - half_h);
    let proj = |lat: f64, lon: f64| -> (f32, f32) {
        (mx0 + mw / 2.0 + (wrap(lon - clon) / k) as f32, my0 + mh / 2.0 - ((lat - clat) / k) as f32)
    };

    // Gradnetz alle 30°
    let half_w = SPAN_LON / 2.0;
    let mut lon = ((clon - half_w) / 30.0).ceil() * 30.0;
    while lon <= clon + half_w {
        let x = proj(0.0, lon).0;
        c.line(Layer::Back, (x, my0), (x, my1), 1.0, cyan(if wrap(lon).abs() < 0.01 { 34 } else { 16 }));
        lon += 30.0;
    }
    for lat in [-60.0, -30.0, 0.0, 30.0, 60.0] {
        let y = proj(lat, clon).1;
        c.line(Layer::Back, (mx0, y), (mx1, y), 1.0, cyan(if lat == 0.0 { 34 } else { 16 }));
    }

    // Landmassen als Punktraster, Nachtseite gedimmt
    let (sdec, slon) = ((d.sun.0 as f64).to_radians(), d.sun.1 as f64);
    let daylight = |lat: f64, lon: f64| {
        let la = lat.to_radians();
        let cz = la.sin() * sdec.sin() + la.cos() * sdec.cos() * (lon - slon).to_radians().cos();
        ((cz + 0.08) / 0.16).clamp(0.0, 1.0) as f32
    };
    let mut lat = ((clat - half_h) / DOT).floor() * DOT;
    while lat <= clat + half_h + DOT {
        let mut lon = ((clon - half_w) / DOT).floor() * DOT;
        while lon <= clon + half_w + DOT {
            let p = proj(lat, lon);
            let day = daylight(lat, lon);
            if is_land(lat, lon) {
                c.disc(Layer::Back, p, 1.3 * s, cyan((55.0 + 150.0 * day) as u8));
            } else {
                c.disc(Layer::Back, p, 0.75 * s, cyan((10.0 + 16.0 * day) as u8));
            }
            lon += DOT;
        }
        lat += DOT;
    }

    // Tag-Nacht-Grenze (Sonnenhöhe 0°); um die Tagundnachtgleiche verläuft sie fast senkrecht – dann weglassen.
    if sdec.abs() > 0.4f64.to_radians() {
        let mut prev: Option<(f32, f32)> = None;
        let steps = (mw / (3.0 * s)).ceil() as i32;
        for i in 0..=steps {
            let lon = clon - half_w + SPAN_LON * i as f64 / steps as f64;
            let lat = (-(sdec.cos() * (lon - slon).to_radians().cos()) / sdec.sin()).atan().to_degrees();
            let p = proj(lat, lon);
            if let Some(q) = prev {
                c.line(Layer::Back, q, p, 1.0, [AMBER[0], AMBER[1], AMBER[2], 110]);
            }
            prev = Some(p);
        }
    }

    match &d.pos {
        Some(p) => draw_iss(&mut c, fonts, s, p, &proj, (mw, mh)),
        None => {
            let l = Line::new(11.0, Weight::Bold).tracking(0.2).span("AWAITING TELEMETRY", cyan(210));
            let lw = text::width(fonts, &l, s);
            c.text(Layer::Fore, fonts, &l, s, mx0 + (mw - lw) / 2.0, my0 + mh / 2.0 - 7.0 * s);
        }
    }
    c.set_clip(None);

    // Rahmen, Nordmarke, Maßstab
    c.corners(Layer::Fore, (mx0 + 0.5, my0 + 0.5, mx1 - 0.5, my1 - 0.5), 10.0 * s, (1.5 * s).max(1.0), cyan(200));
    let (nx, ny) = (mx1 - 14.0 * s, my0 + 10.0 * s);
    c.triangle(Layer::Fore, [(nx, ny - 4.0 * s), (nx - 4.0 * s, ny + 4.0 * s), (nx + 4.0 * s, ny + 4.0 * s)], cyan(220));
    let n = hud::tiny("N", cyan(220));
    let nw = text::width(fonts, &n, s);
    c.text(Layer::Fore, fonts, &n, s, nx - nw / 2.0, ny + 7.0 * s);
    let km_px = (1000.0 / (111.32 * clat.to_radians().cos().max(0.2) * k)) as f32;
    let (bx, by) = (mx0 + 10.0 * s, my1 - 10.0 * s);
    c.line(Layer::Fore, (bx, by), (bx + km_px, by), 1.0, cyan(200));
    for x in [bx, bx + km_px] {
        c.line(Layer::Fore, (x, by - 3.0 * s), (x, by + 1.0 * s), 1.0, cyan(200));
    }
    c.text(Layer::Fore, fonts, &hud::tiny("1000 KM", cyan(200)), s, bx + km_px + 6.0 * s, by - 5.0 * s);

    // Datenzeile
    let y = my1 + 9.0 * s;
    let cols = [0.0, 0.19, 0.39, 0.555, 0.80];
    let values: [(&str, String); 5] = match &d.pos {
        Some(p) => [
            ("LAT", format!("{:.2}° {}", p.lat.abs(), if p.lat < 0.0 { 'S' } else { 'N' })),
            ("LON", format!("{:.2}° {}", p.lon.abs(), if p.lon < 0.0 { 'W' } else { 'E' })),
            ("ALT", format!("{:.0} KM", p.alt_km)),
            ("VELOCITY", format!("{} KM/H", thousands(p.speed_kmh))),
            ("ORBIT", thousands(p.rev)),
        ],
        None => ["LAT", "LON", "ALT", "VELOCITY", "ORBIT"].map(|l| (l, "--".to_string())),
    };
    let value = Line::new(12.0, Weight::Bold).tabular().tracking(0.05);
    for ((label, v), f) in values.iter().zip(cols) {
        let x = iw * s * f;
        c.text(Layer::Fore, fonts, &hud::tiny(label, DIM), s, x, y);
        c.text(Layer::Fore, fonts, &value.clone().span(v.as_str(), SOFT), s, x, y + 12.0 * s);
    }
    c.finish(s)
}

/// Bodenspur, Sichtbarkeitsradius und das ISS-Symbol.
fn draw_iss(c: &mut Canvas, fonts: &Fonts, s: f32, p: &Pos, proj: &dyn Fn(f64, f64) -> (f32, f32), (mw, _mh): (f32, f32)) {
    let cyan = |a: u8| [CYAN[0], CYAN[1], CYAN[2], a];
    // Bodenspur: vergangen durchgezogen und schwach, kommend gestrichelt und hell. Am Kartenrand
    // (Sprung über die Datumsgrenze des Ausschnitts) wird die Linie unterbrochen.
    let pts: Vec<(f32, f32)> = p.track.iter().map(|&(la, lo)| proj(la as f64, lo as f64)).collect();
    for i in 1..pts.len() {
        let (a, b) = (pts[i - 1], pts[i]);
        if (a.0 - b.0).abs() > mw / 2.0 {
            continue;
        }
        if i <= p.past {
            c.line(Layer::Back, a, b, 1.3 * s, cyan(110));
        } else if i % 2 == 0 {
            c.line(Layer::Back, a, b, 1.3 * s, [255, 255, 255, 170]);
        }
    }

    // Sichtbarkeitsradius: bis hier steht die ISS über dem Horizont.
    let lambda = (6371.0 / (6371.0 + p.alt_km as f64)).acos();
    let (phi, lon0) = ((p.lat as f64).to_radians(), p.lon as f64);
    let ring: Vec<(f32, f32)> = (0..=72)
        .map(|i| {
            let th = (i as f64 * 5.0).to_radians();
            let lat2 = (phi.sin() * lambda.cos() + phi.cos() * lambda.sin() * th.cos()).asin();
            let dlon = (th.sin() * lambda.sin() * phi.cos()).atan2(lambda.cos() - phi.sin() * lat2.sin());
            proj(lat2.to_degrees(), lon0 + dlon.to_degrees())
        })
        .collect();
    for i in 1..ring.len() {
        if i % 2 == 0 && (ring[i - 1].0 - ring[i].0).abs() < mw / 2.0 {
            c.line(Layer::Back, ring[i - 1], ring[i], 1.0, cyan(130));
        }
    }

    // ISS: Ringe, Fadenkreuz, Pfeil in Flugrichtung
    let (cx, cy) = proj(p.lat as f64, p.lon as f64);
    c.disc(Layer::Back, (cx, cy), 11.0 * s, cyan(40));
    c.ring(Layer::Fore, (cx, cy), 8.0 * s, (1.4 * s).max(1.0), cyan(230));
    for a in [0.0f32, 90.0, 180.0, 270.0] {
        let (sa, ca) = a.to_radians().sin_cos();
        c.line(Layer::Fore, (cx + ca * 11.0 * s, cy + sa * 11.0 * s), (cx + ca * 15.0 * s, cy + sa * 15.0 * s), 1.2 * s, cyan(200));
    }
    let (sh, ch) = p.heading.sin_cos();
    let dir = (ch, -sh);
    let norm = (-dir.1, dir.0);
    let tip = (cx + dir.0 * 5.5 * s, cy + dir.1 * 5.5 * s);
    let back = (cx - dir.0 * 3.5 * s, cy - dir.1 * 3.5 * s);
    c.triangle(
        Layer::Fore,
        [tip, (back.0 + norm.0 * 4.0 * s, back.1 + norm.1 * 4.0 * s), (back.0 - norm.0 * 4.0 * s, back.1 - norm.1 * 4.0 * s)],
        WHITE,
    );
    c.text(Layer::Fore, fonts, &Line::new(9.0, Weight::Bold).tracking(0.14).span("ISS", WHITE), s, cx + 14.0 * s, cy - 20.0 * s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_mask_knows_the_world() {
        assert!(is_land(52.52, 13.40), "Berlin");
        assert!(is_land(-25.0, 134.0), "Australien");
        assert!(is_land(40.0, -100.0), "USA");
        assert!(!is_land(0.0, -30.0), "Atlantik");
        assert!(!is_land(-40.0, 160.0), "Tasmansee");
        assert!(!is_land(20.0, -150.0), "Pazifik");
        assert!(is_land(-85.0, 0.0), "Antarktis");
    }

    #[test]
    fn content_and_render() {
        let tle = super::orbit::tests::iss();
        let now = tle.epoch as i64 + 1800;
        let d = content(View::Live(tle), now);
        let p = d.pos.as_ref().unwrap();
        assert_eq!(d.status.0, "LIVE");
        assert!((380.0..460.0).contains(&p.alt_km));
        assert!((27_000..28_200).contains(&p.speed_kmh), "{}", p.speed_kmh);
        assert_eq!(p.track.len(), ((TRACK_BACK + TRACK_AHEAD) / TRACK_STEP + 1) as usize);
        assert_eq!(thousands(27_580), "27.580");
        assert_eq!(thousands(580), "580");
        let fonts = Fonts::new();
        let img = render(&d, &fonts, 1.0, WIDTH);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        let waiting = content(View::Waiting, now);
        assert!(waiting.pos.is_none());
        render(&waiting, &fonts, 1.25, WIDTH);
        // Alte Bahndaten werden markiert.
        let old = content(View::Live(super::orbit::tests::iss()), now + 5 * 86_400);
        assert_eq!(old.status.0, "OLD TLE");
    }
}
