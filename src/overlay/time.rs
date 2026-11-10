//! Kalenderrechnung ohne externe Bibliothek: Unix-Zeit ↔ Datum, ISO-8601-Zeitstempel (UTC) lesen.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sonntag … 6 = Samstag
    pub weekday: u32,
}

pub const WEEKDAYS: [&str; 7] = ["SONNTAG", "MONTAG", "DIENSTAG", "MITTWOCH", "DONNERSTAG", "FREITAG", "SAMSTAG"];
pub const WEEKDAYS_SHORT: [&str; 7] = ["SO", "MO", "DI", "MI", "DO", "FR", "SA"];
pub const MONTHS: [&str; 12] =
    ["JANUAR", "FEBRUAR", "MÄRZ", "APRIL", "MAI", "JUNI", "JULI", "AUGUST", "SEPTEMBER", "OKTOBER", "NOVEMBER", "DEZEMBER"];

impl DateTime {
    pub fn new(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Self {
        let weekday = (days_from_civil(year, month, day) + 4).rem_euclid(7) as u32; // 1.1.1970 war ein Donnerstag
        Self { year, month, day, hour, minute, second, weekday }
    }

    pub fn from_unix(t: i64) -> Self {
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400) as u32;
        let (y, m, d) = civil_from_days(days);
        Self::new(y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
    }

    pub fn to_unix(self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86_400 + (self.hour * 3600 + self.minute * 60 + self.second) as i64
    }
}

/// Tage seit 1.1.1970 (Algorithmus von Howard Hinnant, gültig für den gregorianischen Kalender).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

/// `2026-11-12T02:35:00Z` (optional mit Sekundenbruchteilen) → Unix-Sekunden. Nur UTC wird akzeptiert.
pub fn parse_iso_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let s = s.strip_suffix('Z').or_else(|| s.strip_suffix("+00:00"))?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<u32>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<u32>().ok());
    let (hour, minute) = (t.next()??, t.next()??);
    let second = t.next().flatten().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(DateTime::new(year as i32, month, day, hour, minute, second).to_unix())
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for t in [0i64, 951_782_400, 1_700_000_000, 1_794_450_900, 4_102_444_800] {
            assert_eq!(DateTime::from_unix(t).to_unix(), t);
        }
        let d = DateTime::from_unix(0);
        assert_eq!((d.year, d.month, d.day, d.weekday), (1970, 1, 1, 4));
        // 29.02.2000 war ein Dienstag
        let d = DateTime::from_unix(951_782_400);
        assert_eq!((d.year, d.month, d.day, d.weekday), (2000, 2, 29, 2));
    }

    #[test]
    fn iso() {
        let t = parse_iso_utc("2026-11-12T02:35:00Z").unwrap();
        let d = DateTime::from_unix(t);
        assert_eq!((d.year, d.month, d.day, d.hour, d.minute, d.second), (2026, 11, 12, 2, 35, 0));
        assert_eq!(d.weekday, 4, "12.11.2026 ist ein Donnerstag");
        assert_eq!(parse_iso_utc("2026-11-12T02:35:00.123Z"), Some(t));
        assert_eq!(parse_iso_utc("2026-11-12T02:35:00+00:00"), Some(t));
        assert_eq!(parse_iso_utc("2026-11-12T02:35:00+02:00"), None);
        assert_eq!(parse_iso_utc("kaputt"), None);
    }
}
