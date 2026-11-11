//! DSN Now als Mission-Control-Tafel:
//!
//! ```text
//!  DEEP SPACE NETWORK ──────────────────────────── ● 14:32 UTC   ● LIVE
//!  COMPLEX     DISH        LINK  SPACECRAFT      DATA RATE   RTLT       1LS   1LM   1LH  1LD
//!  GOLDSTONE   DSS-14 70M   ◀    VOYAGER 2       160 BPS     1D 14H     ────────────────●──
//!              DSS-26       ◀▶   PERSEVERANCE    2.0 KBPS    10M 53S    ──────────●────────
//!  MADRID      DSS-63 70M   ◀    JAMES WEBB      28.0 MBPS   10.0 S     ─────●─────────────
//!  CANBERRA    STANDBY
//! ```
//!
//! ◀ = Downlink (die Sonde funkt zur Erde), ▶ = Uplink (Kommandos zur Sonde). Die Entfernung steht auf
//! einer logarithmischen Lichtlaufzeit-Skala: Lichtsekunde, -minute, -stunde, -tag.

use super::canvas::{Canvas, Image, Layer};
use super::dsn::{Snapshot, View, STALE};
use super::flight::{fit, AMBER, CYAN, DIM, GREEN, HAIRLINE, RED, SOFT};
use super::hud;
use super::text::{self, Color, Fonts, Line, Weight};

const W: f32 = 760.0;
const ROWS_Y: f32 = 42.0;
const ROW_H: f32 = 22.0;
/// Spalten (x bei Skalierung 1).
const COL: [f32; 7] = [0.0, 110.0, 206.0, 240.0, 432.0, 526.0, 618.0];
/// So viele Verbindungen je Station, danach „+N MORE“.
const PER_STATION: usize = 4;
/// Logarithmische Entfernungsskala in km: 10⁵ … 10^10,6 (Voyager 1 liegt bei ~2,5 · 10¹⁰).
const LOG_MIN: f64 = 5.0;
const LOG_MAX: f64 = 10.6;
const KM_PER_LIGHT_SECOND: f64 = 299_792.458;

#[derive(Debug, Clone, PartialEq)]
struct Row {
    station: String,
    dish: String,
    big: bool,
    craft: String,
    up: bool,
    down: bool,
    rate: String,
    rtlt: String,
    /// Position auf der Entfernungsskala (0 … 1).
    range: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DsnNow {
    status: (&'static str, Color),
    time: String,
    rows: Vec<Row>,
}

pub fn rate(bps: f64) -> String {
    if bps <= 0.0 {
        "CARRIER".into()
    } else if bps < 1000.0 {
        format!("{bps:.0} BPS")
    } else if bps < 1e6 {
        format!("{:.1} KBPS", bps / 1e3)
    } else {
        format!("{:.1} MBPS", bps / 1e6)
    }
}

pub fn light_time(secs: f64) -> String {
    let s = secs.round() as u64;
    if secs < 60.0 {
        format!("{secs:.1} S")
    } else if s < 3600 {
        format!("{}M {:02}S", s / 60, s % 60)
    } else if s < 86_400 {
        format!("{}H {:02}M", s / 3600, s / 60 % 60)
    } else {
        format!("{}D {:02}H", s / 86_400, s / 3600 % 24)
    }
}

fn range_pos(km: f64) -> f32 {
    ((km.log10() - LOG_MIN) / (LOG_MAX - LOG_MIN)).clamp(0.0, 1.0) as f32
}

fn idle(station: &str, text: &str) -> Row {
    Row { station: station.into(), dish: String::new(), big: false, craft: text.into(), up: false, down: false, rate: String::new(), rtlt: String::new(), range: None }
}

pub fn content(view: View, now: i64) -> DsnNow {
    let snap: std::sync::Arc<Snapshot> = match view {
        View::Waiting => {
            return DsnNow { status: ("ACQUIRING", AMBER), time: "--:-- UTC".into(), rows: vec![idle("", "AWAITING TELEMETRY")] }
        }
        View::NoSignal => return DsnNow { status: ("NO SIGNAL", RED), time: "--:-- UTC".into(), rows: vec![idle("", "NO SIGNAL")] },
        View::Live(s) => s,
    };
    // Bewusst UTC: das DSN arbeitet weltweit in UTC.
    let t = super::time::DateTime::from_unix(snap.time);
    let status = if now - snap.time > STALE { ("STALE", AMBER) } else { ("LIVE", GREEN) };
    let mut rows = Vec::new();
    for st in &snap.stations {
        if st.links.is_empty() {
            let text = if st.activity.is_empty() { "STANDBY".to_string() } else { format!("STANDBY · {}", st.activity) };
            rows.push(idle(&st.name, &text));
            continue;
        }
        // Die fernste Sonde zuerst – das ist meist die spannendste.
        let mut links = st.links.clone();
        links.sort_by(|a, b| b.range_km.unwrap_or(0.0).total_cmp(&a.range_km.unwrap_or(0.0)));
        let shown = if links.len() > PER_STATION { PER_STATION - 1 } else { links.len() };
        for (i, l) in links.iter().take(shown).enumerate() {
            // Laufzeit fehlt manchmal – dann aus der Entfernung schätzen.
            let rtlt = l.rtlt.or(l.range_km.map(|km| 2.0 * km / KM_PER_LIGHT_SECOND));
            rows.push(Row {
                station: if i == 0 { st.name.clone() } else { String::new() },
                dish: l.dish.clone(),
                big: l.big,
                craft: l.craft.clone(),
                up: l.up,
                down: l.down,
                rate: rate(l.rate),
                rtlt: rtlt.map(light_time).unwrap_or_else(|| "--".into()),
                range: l.range_km.map(range_pos),
            });
        }
        if links.len() > shown {
            rows.push(idle("", &format!("+{} MORE", links.len() - shown)));
        }
    }
    DsnNow { status, time: format!("{:02}:{:02} UTC", t.hour, t.minute), rows }
}

pub fn render(d: &DsnNow, fonts: &Fonts, s: f32) -> Image {
    let h = ROWS_Y + ROW_H * d.rows.len() as f32 + 2.0;
    let mut c = Canvas::new((W * s).ceil() as u32, (h * s).ceil() as u32, s);
    hud::header(&mut c, fonts, s, (0.0, W * s), 2.0 * s, ("DEEP SPACE NETWORK", CYAN), &[(d.status.0, d.status.1), (&d.time, DIM)]);

    let x = |i: usize| COL[i] * s;
    let head_y = 24.0 * s;
    for (i, label) in [(0, "COMPLEX"), (1, "DISH"), (2, "LINK"), (3, "SPACECRAFT"), (4, "DATA RATE"), (5, "RTLT")] {
        c.text(Layer::Fore, fonts, &hud::tiny(label, DIM), s, x(i), head_y);
    }
    // Entfernungsskala mit Lichtlaufzeit-Marken
    let (bar0, bar1) = (x(6), W * s - 4.0 * s);
    let marks = [(KM_PER_LIGHT_SECOND, "1LS"), (KM_PER_LIGHT_SECOND * 60.0, "1LM"), (KM_PER_LIGHT_SECOND * 3600.0, "1LH"), (KM_PER_LIGHT_SECOND * 86_400.0, "1LD")];
    for (km, label) in marks {
        let mx = bar0 + (bar1 - bar0) * range_pos(km);
        let l = hud::tiny(label, DIM);
        let lw = text::width(fonts, &l, s);
        c.text(Layer::Fore, fonts, &l, s, (mx - lw / 2.0).min(W * s - lw), head_y);
    }

    let value = Line::new(13.0, Weight::Bold).tracking(0.05);
    let mut y = ROWS_Y * s;
    for (i, r) in d.rows.iter().enumerate() {
        if i > 0 && !r.station.is_empty() {
            c.line(Layer::Fore, (0.0, y - 4.0 * s), (W * s, y - 4.0 * s), 1.0, HAIRLINE);
        }
        let mid = y + 8.0 * s;
        let put = |c: &mut Canvas, col: usize, t: &str, color: Color, max: f32| {
            let t = fit(fonts, &value, t, s, max * s);
            c.text(Layer::Fore, fonts, &value.clone().span(t, color), s, x(col), y)
        };
        if !r.station.is_empty() {
            put(&mut c, 0, &r.station, SOFT, COL[1] - COL[0] - 8.0);
        }
        if r.dish.is_empty() {
            // Station ohne Verbindung oder Hinweiszeile
            put(&mut c, 1, &r.craft, DIM, COL[4] - COL[1]);
        } else {
            let dw = put(&mut c, 1, &r.dish, DIM, COL[2] - COL[1]);
            if r.big {
                c.text(Layer::Fore, fonts, &hud::tiny("70M", CYAN), s, x(1) + dw + 5.0 * s, y + 3.0 * s);
            }
            // ◀ Downlink, ▶ Uplink
            let (ax, aw) = (x(2) + 2.0 * s, 6.0 * s);
            if r.down {
                c.triangle(Layer::Fore, [(ax, mid), (ax + aw * 1.4, mid - aw), (ax + aw * 1.4, mid + aw)], GREEN);
            }
            if r.up {
                let bx = ax + 14.0 * s;
                c.triangle(Layer::Fore, [(bx + aw * 1.4, mid), (bx, mid - aw), (bx, mid + aw)], AMBER);
            }
            put(&mut c, 3, &r.craft, SOFT, COL[4] - COL[3] - 8.0);
            put(&mut c, 4, &r.rate, if r.rate == "CARRIER" { DIM } else { SOFT }, COL[5] - COL[4] - 6.0);
            put(&mut c, 5, &r.rtlt, SOFT, COL[6] - COL[5] - 6.0);
            c.line(Layer::Fore, (bar0, mid), (bar1, mid), 1.0, [255, 255, 255, 60]);
            for (km, _) in marks {
                let mx = bar0 + (bar1 - bar0) * range_pos(km);
                c.line(Layer::Fore, (mx, mid - 3.0 * s), (mx, mid + 3.0 * s), 1.0, [255, 255, 255, 110]);
            }
            if let Some(p) = r.range {
                let mx = bar0 + (bar1 - bar0) * p;
                c.line(Layer::Fore, (bar0, mid), (mx, mid), 2.0 * s, [CYAN[0], CYAN[1], CYAN[2], 150]);
                c.disc(Layer::Fore, (mx, mid), 3.5 * s, CYAN);
            }
        }
        y += ROW_H * s;
    }
    c.finish(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::dsn::parse;

    #[test]
    fn formats() {
        assert_eq!(rate(159.99), "160 BPS");
        assert_eq!(rate(2000.0), "2.0 KBPS");
        assert_eq!(rate(28e6), "28.0 MBPS");
        assert_eq!(rate(0.0), "CARRIER");
        assert_eq!(light_time(2.56), "2.6 S");
        assert_eq!(light_time(653.2), "10M 53S");
        assert_eq!(light_time(139_100.5), "1D 14H");
        assert!(range_pos(384_400.0) < range_pos(2.5e10));
        assert_eq!(range_pos(1e3), 0.0);
    }

    #[test]
    fn rows_and_render() {
        let xml = r#"<dsn><station friendlyName="Goldstone"/><dish name="DSS14">
            <downSignal signalType="data" dataRate="160" spacecraft="VGR2"/><target name="VGR2" downlegRange="2.08e10" rtlt="139100"/></dish>
            <station friendlyName="Madrid"/><timestamp>1794450905000</timestamp></dsn>"#;
        let snap = std::sync::Arc::new(parse(xml).unwrap());
        let d = content(View::Live(snap.clone()), snap.time + 30);
        assert_eq!(d.status.0, "LIVE");
        assert_eq!(d.rows.len(), 2);
        assert_eq!((d.rows[0].craft.as_str(), d.rows[1].craft.as_str()), ("VOYAGER 2", "STANDBY"));
        assert_eq!(content(View::Live(snap.clone()), snap.time + STALE + 1).status.0, "STALE");
        let fonts = Fonts::new();
        let img = render(&d, &fonts, 1.0);
        assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);
        let img = render(&content(View::Waiting, 0), &fonts, 1.0);
        assert!(img.height > 2 * img.pad);
    }
}
