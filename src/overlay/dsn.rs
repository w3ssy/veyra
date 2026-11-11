//! DSN Now: wer gerade mit wem funkt – die Live-Daten des NASA Deep Space Network.
//!
//! Die drei Stationen Goldstone, Madrid und Canberra veröffentlichen ihren Antennenstatus als kleine
//! XML-Datei (dieselbe Quelle wie <https://eyes.nasa.gov/dsn/dsn.html>). Ein Hintergrund-Thread holt sie
//! einmal pro Minute – nur solange das Widget aktiv ist, nur lesend, nur von dieser einen Adresse.
//! Es gibt keine offizielle Schnittstellen-Garantie; ändert sich das Format, zeigt das Widget „NO SIGNAL“.

use anyhow::{bail, Context, Result};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HOST: &str = "eyes.nasa.gov";
const PATH: &str = "/dsn/data/dsn.xml";
const REFRESH: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_secs(120);
const MAX_XML: usize = 1024 * 1024;
/// Daten, die älter sind, gelten als veraltet.
pub const STALE: i64 = 10 * 60;

/// Eine aktive Verbindung zwischen Antenne und Raumsonde.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// "DSS-43"
    pub dish: String,
    /// 70-m-Antenne (die drei großen: DSS-14, DSS-43, DSS-63).
    pub big: bool,
    /// Lesbarer Name, z. B. "VOYAGER 2".
    pub craft: String,
    /// Antenne sendet zur Sonde (Uplink).
    pub up: bool,
    /// Antenne empfängt von der Sonde (Downlink).
    pub down: bool,
    /// Höchste Datenrate (Bit/s), Downlink bevorzugt; 0 = nur Träger.
    pub rate: f64,
    /// Entfernung in km (`None` = unbekannt).
    pub range_km: Option<f64>,
    /// Signallaufzeit hin und zurück in Sekunden.
    pub rtlt: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    /// "GOLDSTONE"
    pub name: String,
    pub links: Vec<Link>,
    /// Häufigste Tätigkeit der Antennen ohne Verbindung, z. B. "ENGINEERING UPGRADES" ("" = keine Angabe).
    pub activity: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Zeitstempel der Daten (Unix-Sekunden).
    pub time: i64,
    pub stations: Vec<Station>,
}

pub enum View {
    Waiting,
    NoSignal,
    Live(Arc<Snapshot>),
}

#[derive(Default)]
struct State {
    snapshot: Option<Arc<Snapshot>>,
    error: bool,
}

pub struct Feed {
    state: Arc<Mutex<State>>,
    _stop: Sender<()>,
}

impl Feed {
    pub fn start() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (stop, rx) = channel::<()>();
        let shared = state.clone();
        let spawned = std::thread::Builder::new().name("dsn-now".into()).spawn(move || {
            loop {
                let wait = match fetch() {
                    Ok(snap) => {
                        log::debug!("DSN Now: {} Verbindungen", snap.stations.iter().map(|s| s.links.len()).sum::<usize>());
                        let mut s = shared.lock().unwrap();
                        s.snapshot = Some(Arc::new(snap));
                        s.error = false;
                        REFRESH
                    }
                    Err(e) => {
                        log::warn!("DSN Now: {e:#}");
                        shared.lock().unwrap().error = true;
                        RETRY
                    }
                };
                if !matches!(rx.recv_timeout(wait), Err(RecvTimeoutError::Timeout)) {
                    break;
                }
            }
            log::debug!("DSN Now beendet");
        });
        if let Err(e) = spawned {
            log::error!("DSN Now starten: {e}");
        }
        Self { state, _stop: stop }
    }

    pub fn view(&self) -> View {
        let s = self.state.lock().unwrap();
        match &s.snapshot {
            Some(snap) => View::Live(snap.clone()),
            None if s.error => View::NoSignal,
            None => View::Waiting,
        }
    }
}

fn fetch() -> Result<Snapshot> {
    // Die Datei wird alle paar Sekunden neu geschrieben; der Parameter umgeht Zwischenspeicher unterwegs.
    let path = format!("{PATH}?r={}", super::time::now_unix() / 60);
    let (status, body) = crate::platform::sys::https_get(HOST, &path, MAX_XML)?;
    if status != 200 {
        bail!("{HOST} antwortet mit HTTP {status}");
    }
    parse(&String::from_utf8(body).context("Antwort ist kein UTF-8")?)
}

// ───────────────────────────── XML ─────────────────────────────

/// Ein öffnendes (oder selbstschließendes) Element mit seinen Attributen.
struct Tag<'a> {
    name: &'a str,
    attrs: Vec<(&'a str, &'a str)>,
}

impl Tag<'_> {
    fn get(&self, key: &str) -> &str {
        self.attrs.iter().find(|(k, _)| *k == key).map_or("", |(_, v)| v.trim())
    }

    fn num(&self, key: &str) -> Option<f64> {
        self.get(key).parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
    }
}

/// Minimaler XML-Leser für flache Daten: öffnende Elemente samt Attributen, alles andere wird übersprungen.
/// Keine Entitäten außer den fünf Standard-Kürzeln (die hier nie vorkommen), keine DTDs.
fn tags(xml: &str) -> impl Iterator<Item = Tag<'_>> {
    let mut rest = xml;
    std::iter::from_fn(move || loop {
        let start = rest.find('<')?;
        rest = &rest[start + 1..];
        let end = rest.find('>')?;
        let inner = &rest[..end];
        rest = &rest[end + 1..];
        if inner.starts_with(['/', '?', '!']) {
            continue;
        }
        let inner = inner.strip_suffix('/').unwrap_or(inner);
        let (name, mut a) = inner.split_once(|c: char| c.is_ascii_whitespace()).unwrap_or((inner, ""));
        let mut attrs = Vec::new();
        while let Some(eq) = a.find('=') {
            let key = a[..eq].trim();
            let after = a[eq + 1..].trim_start();
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(close) = after[1..].find(q) else { break };
            attrs.push((key, &after[1..1 + close]));
            a = &after[close + 2..];
        }
        return Some(Tag { name, attrs });
    })
}

#[derive(Default)]
struct Dish {
    name: String,
    activity: String,
    /// (Sonde, Uplink?, Datenrate) – nur aktive Signale
    signals: Vec<(String, bool, f64)>,
    targets: Vec<(String, Option<f64>, Option<f64>)>,
}

pub fn parse(xml: &str) -> Result<Snapshot> {
    if !xml.contains("<dsn") {
        bail!("unerwartetes Format (kein <dsn>)");
    }
    let mut stations: Vec<(String, Vec<Dish>)> = Vec::new();
    for t in tags(xml) {
        match t.name {
            "station" => {
                let name = match t.get("friendlyName") {
                    "" => t.get("name"),
                    n => n,
                };
                stations.push((name.to_uppercase(), Vec::new()));
            }
            "dish" => {
                if let Some((_, dishes)) = stations.last_mut() {
                    dishes.push(Dish { name: t.get("name").to_string(), activity: t.get("activity").to_uppercase(), ..Default::default() });
                }
            }
            "upSignal" | "downSignal" => {
                let Some(dish) = stations.last_mut().and_then(|(_, d)| d.last_mut()) else { continue };
                let kind = t.get("signalType");
                let active = !t.get("active").eq_ignore_ascii_case("false");
                if active && !kind.is_empty() && !kind.eq_ignore_ascii_case("none") {
                    let rate = if kind.eq_ignore_ascii_case("data") { t.num("dataRate").unwrap_or(0.0) } else { 0.0 };
                    dish.signals.push((t.get("spacecraft").to_uppercase(), t.name == "upSignal", rate));
                }
            }
            "target" => {
                let Some(dish) = stations.last_mut().and_then(|(_, d)| d.last_mut()) else { continue };
                let rtlt = t.num("rtlt").filter(|v| *v > 0.0);
                let range = t.num("downlegRange").or_else(|| t.num("uplegRange")).filter(|v| *v > 0.0);
                dish.targets.push((t.get("name").to_uppercase(), range, rtlt));
            }
            _ => {}
        }
    }
    if stations.is_empty() {
        bail!("keine Stationen in den Daten");
    }
    let time = xml
        .split_once("<timestamp>")
        .and_then(|(_, r)| r.split('<').next())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .map(|ms| ms / 1000)
        .unwrap_or_else(super::time::now_unix);

    let stations = stations
        .into_iter()
        .map(|(name, dishes)| {
            let mut links = Vec::new();
            let mut idle: Vec<String> = Vec::new();
            for d in dishes {
                let before = links.len();
                for (code, range_km, rtlt) in &d.targets {
                    if ignored(code) {
                        continue;
                    }
                    let mine = d.signals.iter().filter(|(sc, _, _)| sc == code);
                    let (mut up, mut down, mut up_rate, mut down_rate) = (false, false, 0.0f64, 0.0f64);
                    for (_, is_up, rate) in mine {
                        if *is_up {
                            up = true;
                            up_rate = up_rate.max(*rate);
                        } else {
                            down = true;
                            down_rate = down_rate.max(*rate);
                        }
                    }
                    if !(up || down) {
                        continue;
                    }
                    let num: String = d.name.chars().filter(char::is_ascii_digit).collect();
                    links.push(Link {
                        dish: if num.is_empty() { d.name.to_uppercase() } else { format!("DSS-{num}") },
                        big: matches!(num.as_str(), "14" | "43" | "63"),
                        craft: craft_name(code),
                        up,
                        down,
                        rate: if down_rate > 0.0 { down_rate } else { up_rate },
                        range_km: *range_km,
                        rtlt: *rtlt,
                    });
                }
                if links.len() == before && !d.activity.is_empty() {
                    idle.push(d.activity);
                }
            }
            let activity = idle.iter().max_by_key(|a| idle.iter().filter(|b| b == a).count()).cloned().unwrap_or_default();
            Station { name, links, activity }
        })
        .collect();
    Ok(Snapshot { time, stations })
}

/// Testsignale und Platzhalter, keine echten Raumsonden.
fn ignored(code: &str) -> bool {
    matches!(code, "" | "DSN" | "TEST" | "TST" | "RFC" | "DSS" | "NONE")
}

/// Kürzel aus den DSN-Daten → lesbarer Name. Unbekannte Kürzel bleiben, wie sie sind.
pub fn craft_name(code: &str) -> String {
    let name = match code {
        "VGR1" => "VOYAGER 1",
        "VGR2" => "VOYAGER 2",
        "NHPC" => "NEW HORIZONS",
        "JWST" => "JAMES WEBB",
        "JNO" => "JUNO",
        "EURC" | "ECLP" => "EUROPA CLIPPER",
        "JUCE" | "JUIC" => "JUICE",
        "PSYC" | "PSYCHE" => "PSYCHE",
        "LUCY" => "LUCY",
        "MSL" => "CURIOSITY",
        "M20" | "M2020" => "PERSEVERANCE",
        "MRO" => "MRO",
        "MVN" => "MAVEN",
        "ODY" | "M01O" => "MARS ODYSSEY",
        "TGO" => "EXOMARS TGO",
        "MEX" => "MARS EXPRESS",
        "EMM" => "HOPE",
        "BEPI" => "BEPICOLOMBO",
        "SPP" | "PSP" => "PARKER SOLAR PROBE",
        "SOLO" => "SOLAR ORBITER",
        "STA" => "STEREO-A",
        "SOHO" => "SOHO",
        "ACE" => "ACE",
        "WIND" => "WIND",
        "DSCO" => "DSCOVR",
        "LRO" => "LRO",
        "KPLO" => "DANURI",
        "CAPS" | "CAPSTONE" => "CAPSTONE",
        "ORX" | "ORXA" => "OSIRIS-APEX",
        "HYB2" => "HAYABUSA2",
        "GAIA" => "GAIA",
        "EUCL" => "EUCLID",
        "TESS" => "TESS",
        "MMS" | "MMS1" => "MMS",
        "CHDR" | "CHAN" => "CHANDRA",
        "XMM" => "XMM-NEWTON",
        "ESCB" => "ESCAPADE BLUE",
        "ESCG" => "ESCAPADE GOLD",
        "EM1" | "ORIO" | "ORION" => "ORION",
        _ => code,
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<dsn>
<station name="gdscc" friendlyName="Goldstone" timeUTC="1794450900000" timeZoneOffset="-28800000"/>
<dish name="DSS14" azimuthAngle="160.2" elevationAngle="30.1" isMSPA="false" isArray="false" isDDOR="false">
  <upSignal signalType="none" dataRate="" power="0" spacecraft="VGR2" spacecraftID="-32"/>
  <downSignal signalType="data" dataRate="159.9999" frequency="8420000000" power="-158.5" spacecraft="VGR2" spacecraftID="-32"/>
  <target name="VGR2" id="32" uplegRange="20850000000" downlegRange="20850100000" rtlt="139100.5"/>
</dish>
<dish name="DSS26" isMSPA="true">
  <upSignal signalType="data" dataRate="2000" spacecraft="M20" active="true"/>
  <downSignal signalType="carrier" dataRate="0" spacecraft="M20"/>
  <downSignal signalType="data" dataRate="2048000" spacecraft="MRO"/>
  <target name="M20" uplegRange="98000000" downlegRange="98000000" rtlt="653.2"/>
  <target name="MRO" uplegRange="98000000" downlegRange="-1" rtlt="-1"/>
</dish>
<station name="mdscc" friendlyName="Madrid" timeUTC="1794450900000"/>
<dish name="DSS63"><target name="DSN" rtlt="-1"/><downSignal signalType="data" spacecraft="DSN" dataRate="1"/></dish>
<station name="cdscc" friendlyName="Canberra"/>
<dish name="DSS35" activity="Engineering Upgrades"><target name="DSN" id="99" uplegRange="-1" downlegRange="-1" rtlt="-1"/></dish>
<dish name="DSS43">
  <downSignal signalType="none" spacecraft="VGR1"/>
  <target name="VGR1" rtlt="164000"/>
</dish>
<timestamp>1794450905123</timestamp>
</dsn>"#;

    #[test]
    fn parses_links() {
        let s = parse(SAMPLE).unwrap();
        assert_eq!(s.time, 1_794_450_905);
        let names: Vec<&str> = s.stations.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["GOLDSTONE", "MADRID", "CANBERRA"]);
        let g = &s.stations[0].links;
        assert_eq!(g.len(), 3);
        let vgr = &g[0];
        assert_eq!((vgr.dish.as_str(), vgr.big, vgr.craft.as_str(), vgr.up, vgr.down), ("DSS-14", true, "VOYAGER 2", false, true));
        assert!((vgr.rate - 160.0).abs() < 0.01);
        assert_eq!(vgr.rtlt, Some(139_100.5));
        let m20 = &g[1];
        assert_eq!((m20.craft.as_str(), m20.up, m20.down, m20.rate), ("PERSEVERANCE", true, true, 2000.0));
        let mro = &g[2];
        assert_eq!((mro.rate, mro.range_km, mro.rtlt), (2_048_000.0, Some(98_000_000.0), None));
        assert!(s.stations[1].links.is_empty(), "Testsignal wird ignoriert");
        assert!(s.stations[2].links.is_empty(), "Sonde ohne aktives Signal wird nicht gezeigt");
        assert_eq!(s.stations[2].activity, "ENGINEERING UPGRADES");
        assert_eq!(s.stations[0].activity, "");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("<html>Wartung</html>").is_err());
        assert!(parse("<dsn></dsn>").is_err());
        // Kaputte Tags dürfen nicht hängen bleiben oder abstürzen.
        let s = parse("<dsn><station name=\"x\" friendlyName=\"A\"/><dish name=\"DSS1\"><target name='VGR1' rtlt=\"5").unwrap();
        assert_eq!(s.stations.len(), 1);
    }
}
