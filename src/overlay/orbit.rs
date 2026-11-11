//! Bahnrechnung für den ISS Tracker – ohne Bibliothek, aus den öffentlichen Bahnelementen (TLE).
//!
//! Die TLE beschreibt eine mittlere Bahn. Daraus wird hier eine Kepler-Ellipse, die sich durch die
//! Abplattung der Erde (J2: Drehung von Knotenlinie und Perigäum) und die Abbremsung in der Hochatmosphäre
//! (ṅ/2 aus der TLE) langsam verändert. Kurzperiodische Schwankungen fehlen – die Position stimmt damit auf
//! einige zehn Kilometer, solange die TLE nur Stunden bis wenige Tage alt ist. Für eine Karte mit
//! ~35 km pro Pixel ist das mehr als genau genug.

use super::astro;
use super::time::DateTime;
use std::f64::consts::{PI, TAU};

/// Gravitationsparameter der Erde (km³/s²), Äquatorradius (km), J2 – wie in WGS-72/SGP4.
const MU: f64 = 398_600.8;
const RE: f64 = 6378.135;
const J2: f64 = 0.001_082_616;
/// WGS-84 für die Umrechnung in geografische Breite und Höhe.
const WGS_A: f64 = 6378.137;
const WGS_E2: f64 = 0.006_694_379_990_14;

#[derive(Debug, Clone, PartialEq)]
pub struct Tle {
    /// Zeitpunkt der Elemente (Unix-Sekunden, UTC).
    pub epoch: f64,
    /// Mittlere Bewegung in Umläufen pro Tag.
    pub rev_per_day: f64,
    /// ṅ/2 in Umläufen pro Tag².
    pub ndot2: f64,
    pub ecc: f64,
    /// Winkel in Bogenmaß.
    pub incl: f64,
    pub raan: f64,
    pub argp: f64,
    pub mean_anomaly: f64,
    /// Umlaufnummer zur Epoche.
    pub rev: u32,
}

fn checksum_ok(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 69 || !b[68].is_ascii_digit() {
        return false;
    }
    let sum: u32 = b[..68]
        .iter()
        .map(|c| match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'-' => 1,
            _ => 0,
        })
        .sum();
    sum % 10 == (b[68] - b'0') as u32
}

/// Zwei-Zeilen-Elemente lesen (eine optionale Namenszeile davor wird übersprungen).
pub fn parse(text: &str) -> Option<Tle> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let i = lines.iter().position(|l| l.starts_with("1 ") && l.len() >= 69)?;
    let (l1, l2) = (lines[i], *lines.get(i + 1)?);
    if !l2.starts_with("2 ") || !checksum_ok(l1) || !checksum_ok(l2) || l1.get(2..7)? != l2.get(2..7)? {
        return None;
    }
    let f = |l: &str, a: usize, b: usize| l.get(a..b)?.trim().parse::<f64>().ok();
    let yy = f(l1, 18, 20)? as i32;
    let year = if yy < 57 { 2000 + yy } else { 1900 + yy };
    let day = f(l1, 20, 32)?;
    let epoch = DateTime::new(year, 1, 1, 0, 0, 0).to_unix() as f64 + (day - 1.0) * 86_400.0;
    let ndot2 = f(l1, 33, 43)?;
    let deg = |a, b| f(l2, a, b).map(f64::to_radians);
    let tle = Tle {
        epoch,
        rev_per_day: f(l2, 52, 63)?,
        ndot2,
        ecc: format!("0.{}", l2.get(26..33)?.trim()).parse().ok()?,
        incl: deg(8, 16)?,
        raan: deg(17, 25)?,
        argp: deg(34, 42)?,
        mean_anomaly: deg(43, 51)?,
        rev: l2.get(63..68).and_then(|s| s.trim().parse().ok()).unwrap_or(0),
    };
    (tle.rev_per_day > 0.5 && tle.ecc < 0.99).then_some(tle)
}

/// Zustand zu einem Zeitpunkt.
#[derive(Debug, Clone, Copy)]
pub struct State {
    /// Position im erdfesten Inertialsystem (TEME, km).
    pub eci: [f64; 3],
    pub lat: f64,
    pub lon: f64,
    pub alt_km: f64,
    pub speed_kms: f64,
    /// Umlaufnummer (vollendete Umläufe seit dem Start).
    pub rev: u32,
}

impl Tle {
    /// Position zur Unix-Zeit `t`.
    pub fn at(&self, t: f64) -> State {
        let cos_i = self.incl.cos();
        let beta2 = 1.0 - self.ecc * self.ecc;
        // Mittlere Bewegung aus der TLE (Kozai) → Brouwer, wie SGP4.
        let n_kozai = self.rev_per_day * TAU / 86_400.0;
        let a1 = (MU / (n_kozai * n_kozai)).cbrt();
        let k = 0.75 * J2 * (RE * RE) * (3.0 * cos_i * cos_i - 1.0) / beta2.powf(1.5);
        let d1 = k / (a1 * a1);
        let a0 = a1 * (1.0 - d1 / 3.0 - d1 * d1 - 134.0 / 81.0 * d1 * d1 * d1);
        let d0 = k / (a0 * a0);
        let n = n_kozai / (1.0 + d0);
        let a = a0 / (1.0 - d0);

        // Säkulare Störungen durch J2.
        let p = a * beta2;
        let j = 1.5 * J2 * (RE / p).powi(2) * n;
        let dt = t - self.epoch;
        let days = dt / 86_400.0;
        let raan = self.raan - j * cos_i * dt;
        let argp = self.argp + 0.5 * j * (5.0 * cos_i * cos_i - 1.0) * dt;
        let m_rate = n + 0.5 * j * beta2.sqrt() * (3.0 * cos_i * cos_i - 1.0);
        // Abbremsung: ṅ/2 · Δt² Umläufe zusätzlich.
        let mean = self.mean_anomaly + m_rate * dt + TAU * self.ndot2 * days * days;
        // Umläufe werden am aufsteigenden Knoten gezählt (Argument der Breite überschreitet 360°).
        let lat_arg = |argp: f64, mean: f64| ((argp + mean) / TAU).floor();
        let revs = self.rev as f64 + lat_arg(argp, mean) - lat_arg(self.argp, self.mean_anomaly);

        // Kepler-Gleichung lösen.
        let m = mean.rem_euclid(TAU);
        let mut e = if self.ecc < 0.8 { m } else { PI };
        for _ in 0..12 {
            let d = (e - self.ecc * e.sin() - m) / (1.0 - self.ecc * e.cos());
            e -= d;
            if d.abs() < 1e-12 {
                break;
            }
        }
        let nu = 2.0 * ((1.0 + self.ecc).sqrt() * (e / 2.0).sin()).atan2((1.0 - self.ecc).sqrt() * (e / 2.0).cos());
        let r = a * (1.0 - self.ecc * e.cos());
        let u = argp + nu;
        let (su, cu) = u.sin_cos();
        let (so, co) = raan.sin_cos();
        let si = self.incl.sin();
        let eci = [r * (co * cu - so * su * cos_i), r * (so * cu + co * su * cos_i), r * su * si];

        // Erdfest: um die Sternzeit zurückdrehen.
        let theta = astro::gmst_hours(t) / 24.0 * TAU;
        let (st, ct) = theta.sin_cos();
        let (x, y, z) = (ct * eci[0] + st * eci[1], -st * eci[0] + ct * eci[1], eci[2]);
        let (lat, alt_km) = geodetic(x, y, z);
        State {
            eci,
            lat: lat.to_degrees(),
            lon: y.atan2(x).to_degrees(),
            alt_km,
            speed_kms: (MU * (2.0 / r - 1.0 / a)).sqrt(),
            rev: revs.max(0.0) as u32,
        }
    }
}

/// Erdfeste Koordinaten → geografische Breite (Bogenmaß) und Höhe über WGS-84 (km).
fn geodetic(x: f64, y: f64, z: f64) -> (f64, f64) {
    let p = x.hypot(y);
    let mut lat = z.atan2(p * (1.0 - WGS_E2));
    let mut h = 0.0;
    for _ in 0..5 {
        let n = WGS_A / (1.0 - WGS_E2 * lat.sin().powi(2)).sqrt();
        h = p / lat.cos() - n;
        lat = z.atan2(p * (1.0 - WGS_E2 * n / (n + h)));
    }
    (lat, h)
}

/// Sonne: Bildpunkt (Breite, Länge in Grad) und Richtung im Inertialsystem (Einheitsvektor).
/// Formel des Astronomical Almanac, auf ~0,01° genau.
pub fn sun(t: f64) -> ((f64, f64), [f64; 3]) {
    let d = astro::julian_date(t) - 2_451_545.0;
    let g = (357.529 + 0.985_600_28 * d).to_radians();
    let q = 280.459 + 0.985_647_36 * d;
    let l = (q + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_36 * d).to_radians();
    let dir = [l.cos(), eps.cos() * l.sin(), eps.sin() * l.sin()];
    let ra = dir[1].atan2(dir[0]).to_degrees();
    let dec = dir[2].asin().to_degrees();
    let lon = (ra - astro::gmst_hours(t) * 15.0 + 540.0).rem_euclid(360.0) - 180.0;
    ((dec, lon), dir)
}

/// Liegt die Position im Erdschatten (zylindrisches Schattenmodell)?
pub fn in_shadow(eci: [f64; 3], sun_dir: [f64; 3]) -> bool {
    let along = eci[0] * sun_dir[0] + eci[1] * sun_dir[1] + eci[2] * sun_dir[2];
    if along >= 0.0 {
        return false;
    }
    let perp = [eci[0] - along * sun_dir[0], eci[1] - along * sun_dir[1], eci[2] - along * sun_dir[2]];
    (perp[0] * perp[0] + perp[1] * perp[1] + perp[2] * perp[2]).sqrt() < WGS_A
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Testfall aus Vallado et al. (2006), „Revisiting Spacetrack Report #3“ (Satellit 00005).
    const VANGUARD: &str = "1 00005U 58002B   00179.78495062  .00000023  00000-0  28098-4 0  4753
2 00005  34.2682 348.7242 1859667 331.7664  19.3264 10.82419157413667";

    pub const ISS: &str = "ISS (ZARYA)
1 25544U 98067A   26315.50000000  .00012000  00000+0  21000-3 0  9990
2 25544  51.6400 120.0000 0004000  80.0000 270.0000 15.50000000500000";

    fn fix_checksums(text: &str) -> String {
        text.lines()
            .map(|l| {
                if l.len() < 69 {
                    return l.to_string();
                }
                let sum: u32 = l.as_bytes()[..68]
                    .iter()
                    .map(|c| match c {
                        b'0'..=b'9' => (c - b'0') as u32,
                        b'-' => 1,
                        _ => 0,
                    })
                    .sum();
                format!("{}{}", &l[..68], sum % 10)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn iss() -> Tle {
        parse(&fix_checksums(ISS)).unwrap()
    }

    #[test]
    fn parses_and_checks() {
        let t = parse(VANGUARD).unwrap();
        assert!((t.incl.to_degrees() - 34.2682).abs() < 1e-9);
        assert!((t.ecc - 0.185_966_7).abs() < 1e-12);
        assert!((t.rev_per_day - 10.824_191_57).abs() < 1e-9);
        assert_eq!(t.rev, 41366);
        // Tag 179,78495062 im Jahr 2000 = 27.6.2000, 18:50:19,73 UTC
        assert!((t.epoch - 962_131_819.73).abs() < 0.01, "{}", t.epoch);
        // Eine verfälschte Ziffer fällt durch die Prüfsumme.
        assert!(parse(&VANGUARD.replace("34.2682", "34.2683")).is_none());
        assert!(parse("Unsinn").is_none());
    }

    #[test]
    fn matches_sgp4_reference() {
        // SGP4-Referenz zur Epoche: r = (7022.465, −1400.083, 0.040) km. Ohne kurzperiodische Terme
        // liegt das Modell hier (stark exzentrische Bahn) rund 8 km daneben.
        let t = parse(VANGUARD).unwrap();
        let s = t.at(t.epoch);
        let d = ((s.eci[0] - 7022.465).powi(2) + (s.eci[1] + 1400.083).powi(2) + (s.eci[2] - 0.040).powi(2)).sqrt();
        assert!(d < 15.0, "Abweichung {d:.1} km, {:?}", s.eci);
    }

    #[test]
    fn iss_orbit_is_plausible() {
        let t = iss();
        let mut max_lat = 0.0f64;
        for k in 0..200 {
            let s = t.at(t.epoch + k as f64 * 60.0);
            assert!((380.0..460.0).contains(&s.alt_km), "Höhe {}", s.alt_km);
            assert!((7.5..7.8).contains(&s.speed_kms), "Geschwindigkeit {}", s.speed_kms);
            max_lat = max_lat.max(s.lat.abs());
        }
        // Größte Breite ≈ Bahnneigung
        assert!((51.0..52.2).contains(&max_lat), "{max_lat}");
        // Nach einem Umlauf (~92,9 min) wieder fast an derselben Stelle im Raum
        let period = 86_400.0 / t.rev_per_day;
        let (a, b) = (t.at(t.epoch).eci, t.at(t.epoch + period).eci);
        let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        assert!(d < 150.0, "{d}");
        // 10° vor dem aufsteigenden Knoten: nach 2,5 Umläufen dreimal über den Äquator nach Norden.
        assert_eq!(t.at(t.epoch).rev, 50000);
        assert_eq!(t.at(t.epoch + period * 2.5).rev, 50003);
    }

    #[test]
    fn sun_and_shadow() {
        // Sommersonnenwende 2026 (21.6., ~08:24 UTC): Deklination ≈ +23,44°
        let ((dec, _), dir) = sun(DateTime::new(2026, 6, 21, 8, 24, 0).to_unix() as f64);
        assert!((dec - 23.44).abs() < 0.05, "{dec}");
        // Mittags in Greenwich steht die Sonne nahe Länge 0
        let ((_, lon), _) = sun(DateTime::new(2026, 3, 20, 12, 0, 0).to_unix() as f64);
        assert!(lon.abs() < 2.5, "{lon}");
        let r = 6800.0;
        assert!(!in_shadow(dir.map(|v| v * r), dir));
        assert!(in_shadow(dir.map(|v| -v * r), dir));
        let side = [-dir[1], dir[0], 0.0];
        let norm = (side[0] * side[0] + side[1] * side[1]).sqrt();
        assert!(!in_shadow([side[0] / norm * r, side[1] / norm * r, 0.0], dir), "seitlich der Erde ist es hell");
    }
}
