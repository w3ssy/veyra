//! Flight Control: die nächsten Raketenstarts aus der Launch Library 2 (The Space Devs).
//!
//! Die einzige Stelle in Veyra mit Netzwerkzugriff – und nur, solange das Widget aktiv ist.
//! Ein Hintergrund-Thread fragt alle 15 Minuten eine fest eingebaute Adresse ab (HTTPS, nur lesend,
//! deutlich unter der Grenze von 15 Abfragen pro Stunde ohne API-Schlüssel). Die Antwort wird als
//! `launches.json` zwischengespeichert: Nach einem Neustart ist sofort etwas zu sehen, auch offline.

use super::time::parse_iso_utc;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const HOST: &str = "ll.thespacedevs.com";
const PATH: &str = "/2.3.0/launches/upcoming/?limit=10&mode=normal&hide_recent_previous=true";
const REFRESH: Duration = Duration::from_secs(15 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);
const RATE_LIMITED: Duration = Duration::from_secs(30 * 60);
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// So lange nach T-0 bleibt ein Start (mit T+) stehen.
pub const AFTER_LIFTOFF: i64 = 20 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub rocket: String,
    pub mission: String,
    pub orbit: String,
    /// Startzeit (Unix-Sekunden, UTC).
    pub net: i64,
    /// Startzeit mindestens auf die Stunde genau → Countdown; sonst nur das Datum.
    pub precise: bool,
    /// Kurzform: "Go", "TBC", "TBD", "Hold", "In Flight", "Success" …
    pub status: String,
    pub pad: String,
    pub location: String,
    pub webcast: bool,
}

#[derive(Default)]
struct State {
    launches: Vec<Launch>,
    /// Letzter Abruf fehlgeschlagen.
    error: bool,
}

pub enum View {
    /// Noch keine Daten (erster Abruf läuft).
    Waiting,
    /// Keine Daten und der Abruf schlägt fehl.
    NoSignal,
    Launch(Launch),
}

pub struct Feed {
    state: Arc<Mutex<State>>,
    /// Wird die `Feed` verworfen, endet der Thread beim nächsten Aufwachen.
    _stop: Sender<()>,
}

impl Feed {
    pub fn start(cache: PathBuf) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (stop, rx) = channel::<()>();
        let shared = state.clone();
        let spawned = std::thread::Builder::new().name("flight-control".into()).spawn(move || {
            let mut delay = Duration::ZERO;
            if let Ok(text) = std::fs::read_to_string(&cache) {
                match parse(&text) {
                    Ok(list) => {
                        shared.lock().unwrap().launches = list;
                        let age = std::fs::metadata(&cache).and_then(|m| m.modified()).ok().and_then(|m| SystemTime::now().duration_since(m).ok());
                        delay = REFRESH.saturating_sub(age.unwrap_or(REFRESH));
                    }
                    Err(e) => log::warn!("Flight Control: Zwischenspeicher unbrauchbar: {e:#}"),
                }
            }
            // Aufwachen nach `delay` – oder sofort beenden, wenn die `Feed` verworfen wurde.
            while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(delay) {
                delay = match fetch() {
                    Ok((text, list)) => {
                        log::info!("Flight Control: {} kommende Starts geladen", list.len());
                        if let Err(e) = std::fs::write(&cache, text) {
                            log::warn!("Flight Control: {cache:?} schreiben: {e}");
                        }
                        let mut s = shared.lock().unwrap();
                        s.launches = list;
                        s.error = false;
                        REFRESH
                    }
                    Err(e) => {
                        log::warn!("Flight Control: {e:#}");
                        shared.lock().unwrap().error = true;
                        if format!("{e}").contains("429") {
                            RATE_LIMITED
                        } else {
                            RETRY
                        }
                    }
                };
            }
            log::debug!("Flight Control beendet");
        });
        if let Err(e) = spawned {
            log::error!("Flight Control starten: {e}");
        }
        Self { state, _stop: stop }
    }

    /// Der Start, der gerade angezeigt werden soll.
    pub fn view(&self, now: i64) -> View {
        let s = self.state.lock().unwrap();
        match next(&s.launches, now) {
            Some(l) => View::Launch(l.clone()),
            None if s.error => View::NoSignal,
            None => View::Waiting,
        }
    }
}

/// Erster Start, der noch bevorsteht (oder gerade erst abgehoben ist).
fn next(list: &[Launch], now: i64) -> Option<&Launch> {
    list.iter()
        .filter(|l| !matches!(l.status.as_str(), "Success" | "Failure" | "Partial Failure"))
        .find(|l| l.net + AFTER_LIFTOFF > now)
}

fn fetch() -> Result<(String, Vec<Launch>)> {
    let (status, body) = crate::platform::sys::https_get(HOST, PATH, MAX_BYTES)?;
    if status != 200 {
        bail!("{HOST} antwortet mit HTTP {status}");
    }
    let text = String::from_utf8(body).context("Antwort ist kein UTF-8")?;
    let list = parse(&text)?;
    Ok((text, list))
}

fn parse(text: &str) -> Result<Vec<Launch>> {
    let v: Value = serde_json::from_str(text).context("Antwort ist kein JSON")?;
    let results = v.get("results").and_then(Value::as_array).context("Feld \"results\" fehlt")?;
    let mut list: Vec<Launch> = results.iter().filter_map(launch).collect();
    list.sort_by_key(|l| l.net);
    Ok(list)
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    path.iter().try_fold(v, |v, k| v.get(k)).and_then(Value::as_str).unwrap_or("").trim()
}

fn launch(v: &Value) -> Option<Launch> {
    let net = parse_iso_utc(str_at(v, &["net"]))?;
    let name = str_at(v, &["name"]);
    // "Falcon 9 Block 5 | Starlink Group 12-7" → Mission steht hinter dem Strich.
    let mission = match name.split_once(" | ") {
        Some((_, m)) => m.trim(),
        None if !str_at(v, &["mission", "name"]).is_empty() => str_at(v, &["mission", "name"]),
        None => name,
    };
    let rocket = match str_at(v, &["rocket", "configuration", "name"]) {
        "" => name.split(" | ").next().unwrap_or(""),
        r => r,
    };
    let precision = str_at(v, &["net_precision", "name"]);
    let orbit = str_at(v, &["mission", "orbit", "abbrev"]);
    Some(Launch {
        rocket: rocket.to_string(),
        mission: mission.to_string(),
        orbit: if orbit == "N/A" { String::new() } else { orbit.to_string() },
        net,
        precise: matches!(precision, "" | "Second" | "Minute" | "Hour"),
        status: str_at(v, &["status", "abbrev"]).to_string(),
        pad: short_pad(str_at(v, &["pad", "name"])),
        location: str_at(v, &["pad", "location", "name"]).split(',').next().unwrap_or("").trim().to_string(),
        webcast: v.get("webcast_live").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// "Space Launch Complex 40" → "SLC-40", "Launch Area 94 (SLS-2 / 603)" → "LA-94".
fn short_pad(name: &str) -> String {
    let name = name.split(" (").next().unwrap_or(name).trim();
    for (long, short) in [("Space Launch Complex ", "SLC-"), ("Launch Complex ", "LC-"), ("Launch Area ", "LA-"), ("Launch Pad ", "LP-")] {
        if let Some(rest) = name.strip_prefix(long) {
            return format!("{short}{rest}");
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"count":2,"results":[
        {"name":"Falcon 9 Block 5 | USSF-627","net":"2026-11-12T02:35:00Z",
         "status":{"abbrev":"Go"},"net_precision":{"name":"Minute"},"webcast_live":false,
         "rocket":{"configuration":{"name":"Falcon 9"}},
         "mission":{"name":"USSF-627","orbit":{"abbrev":"LEO"}},
         "pad":{"name":"Space Launch Complex 40","location":{"name":"Cape Canaveral SFS, FL, USA"}}},
        {"name":"Long March 2D | Unknown Payload","net":"2026-11-11T11:40:00Z",
         "status":{"abbrev":"TBC"},"net_precision":{"name":"Day"},
         "rocket":{"configuration":{"name":"Long March 2D"}},
         "mission":{"name":"Unknown Payload","orbit":{"abbrev":"N/A"}},
         "pad":{"name":"Launch Area 94 (SLS-2 / 603)","location":{"name":"Jiuquan Satellite Launch Center, People's Republic of China"}}},
        {"name":"kaputt","net":"irgendwann"}
    ]}"#;

    #[test]
    fn parses_and_sorts() {
        let list = parse(SAMPLE).unwrap();
        assert_eq!(list.len(), 2, "Eintrag ohne gültige Zeit wird übersprungen");
        let lm = &list[0];
        assert_eq!((lm.rocket.as_str(), lm.mission.as_str(), lm.pad.as_str()), ("Long March 2D", "Unknown Payload", "LA-94"));
        assert_eq!(lm.location, "Jiuquan Satellite Launch Center");
        assert!(!lm.precise && lm.orbit.is_empty());
        let f9 = &list[1];
        assert_eq!((f9.pad.as_str(), f9.location.as_str(), f9.orbit.as_str()), ("SLC-40", "Cape Canaveral SFS", "LEO"));
        assert!(f9.precise);
    }

    #[test]
    fn picks_next_launch() {
        let list = parse(SAMPLE).unwrap();
        let t_lm = list[0].net;
        assert_eq!(next(&list, t_lm - 60).unwrap().rocket, "Long March 2D");
        assert_eq!(next(&list, t_lm + 60).unwrap().rocket, "Long March 2D", "kurz nach T-0 noch mit T+ sichtbar");
        assert_eq!(next(&list, t_lm + AFTER_LIFTOFF + 1).unwrap().rocket, "Falcon 9");
        assert!(next(&list, list[1].net + AFTER_LIFTOFF).is_none());
    }
}
