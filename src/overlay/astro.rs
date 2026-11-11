//! Zeitrechnung für die Deep Space Clock – reine Formeln, kein Netzwerk, keine Tabellen.
//!
//! - Julianisches Datum: fortlaufende Tageszählung der Astronomie (Tageswechsel mittags).
//! - Sternzeit (GMST/LST): Drehwinkel der Erde gegenüber den Sternen, nach IAU 1982 (genau auf ~0,1 s).
//! - Mars-Zeit nach Allison & McEwen (2000), wie NASA GISS „Mars24“: Mars Sol Date (MSD),
//!   Coordinated Mars Time (MTC) und die mittlere Ortszeit (LMST) an den Rover-Standorten.

/// Julianisches Datum des Unix-Nullpunkts (1.1.1970, 00:00 UTC).
const UNIX_EPOCH_JD: f64 = 2_440_587.5;
/// TT − UTC: 32,184 s plus 37 Schaltsekunden (seit 2017 unverändert).
const TT_MINUS_UTC: f64 = 69.184;
/// Länge eines Sols in Erdtagen.
pub const SOL_DAYS: f64 = 1.027_491_251_7;

pub fn julian_date(unix: f64) -> f64 {
    unix / 86_400.0 + UNIX_EPOCH_JD
}

/// Mittlere Greenwich-Sternzeit in Stunden (0 … 24).
pub fn gmst_hours(unix: f64) -> f64 {
    let d = julian_date(unix) - 2_451_545.0;
    (18.697_374_558 + 24.065_709_824_419_08 * d).rem_euclid(24.0)
}

/// Örtliche Sternzeit in Stunden; `lon_east` in Grad (Osten positiv).
pub fn lst_hours(unix: f64, lon_east: f64) -> f64 {
    (gmst_hours(unix) + lon_east / 15.0).rem_euclid(24.0)
}

/// Mars Sol Date: fortlaufende Sol-Zählung (Sol 0 begann am 29.12.1873).
pub fn mars_sol_date(unix: f64) -> f64 {
    let jd_tt = julian_date(unix) + TT_MINUS_UTC / 86_400.0;
    (jd_tt - 2_405_522.002_877_9) / SOL_DAYS
}

/// Coordinated Mars Time (mittlere Sonnenzeit am Null-Meridian des Mars) in Stunden.
pub fn mtc_hours(unix: f64) -> f64 {
    (24.0 * mars_sol_date(unix)).rem_euclid(24.0)
}

/// Mittlere Ortszeit auf dem Mars (LMST) in Stunden; `lon_east` in Grad Ost.
pub fn lmst_hours(unix: f64, lon_east: f64) -> f64 {
    (mtc_hours(unix) + lon_east / 15.0).rem_euclid(24.0)
}

/// Missions-Sol am Standort: Sol 0 ist der Landetag (Ortszeit), danach wird um Mitternacht LMST weitergezählt.
pub fn mission_sol(unix: f64, lon_east: f64, landing_unix: f64) -> i64 {
    let local = |t: f64| (mars_sol_date(t) + lon_east / 360.0).floor() as i64;
    local(unix) - local(landing_unix)
}

/// Ein Mars-Rover mit festem Standort.
pub struct Rover {
    pub name: &'static str,
    /// Grad Ost (planetozentrisch).
    pub lon_east: f64,
    /// Landung (Unix-Sekunden, UTC).
    pub landing: f64,
}

pub const ROVERS: [Rover; 2] = [
    // Gale-Krater, gelandet 6.8.2012 05:17:57 UTC
    Rover { name: "CURIOSITY", lon_east: 137.4417, landing: 1_344_230_277.0 },
    // Jezero-Krater, gelandet 18.2.2021 20:55:00 UTC
    Rover { name: "PERSEVERANCE", lon_east: 77.4509, landing: 1_613_681_700.0 },
];

/// Stunden (0 … 24) → (h, min, s).
pub fn hms(hours: f64) -> (u32, u32, u32) {
    let s = (hours.rem_euclid(24.0) * 3600.0).floor() as u32 % 86_400;
    (s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn julian_date_epochs() {
        assert_eq!(julian_date(0.0), 2_440_587.5);
        // J2000.0 = 1.1.2000, 12:00 UTC
        assert!((julian_date(946_728_000.0) - 2_451_545.0).abs() < 1e-9);
    }

    #[test]
    fn sidereal_time() {
        // An J2000.0 per Definition 18,697374558 h = 18:41:50.5
        assert_eq!(hms(gmst_hours(946_728_000.0)), (18, 41, 50));
        // Ein Sterntag ist 23 h 56 min 4,09 s lang.
        let t = 1_794_450_900.0;
        let day = 86_164.090_5;
        assert!((gmst_hours(t + day) - gmst_hours(t)).abs() < 1e-4);
        // 15° östlich = eine Stunde später
        assert!(((lst_hours(t, 15.0) - gmst_hours(t)).rem_euclid(24.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn mars_time() {
        // Mars24, Beispiel A (6.1.2000, 00:00 UTC; damals TT−UTC = 64,184 s – hier 5 s mehr): MSD ≈ 44795,9998
        let msd = mars_sol_date(947_116_800.0);
        assert!((msd - 44_795.999_8).abs() < 0.0005, "{msd}");
        // Ein Sol dauert 24 h 39 min 35,244 s.
        let t = 1_794_450_900.0;
        assert!((mars_sol_date(t + 88_775.244) - mars_sol_date(t) - 1.0).abs() < 1e-6);
        assert_eq!(mission_sol(ROVERS[0].landing + 1.0, ROVERS[0].lon_east, ROVERS[0].landing), 0);
        // Gut 1000 Sols nach der Landung: Sol 1000 bzw. 1001 je nach Tageszeit der Landung.
        let p = &ROVERS[1];
        let s = mission_sol(p.landing + 1000.0 * SOL_DAYS * 86_400.0 + 60.0, p.lon_east, p.landing);
        assert!(s == 1000, "{s}");
        let c = &ROVERS[0];
        assert_eq!(mission_sol(c.landing + 4000.0 * SOL_DAYS * 86_400.0 + 60.0, c.lon_east, c.landing), 4000);
    }
}
