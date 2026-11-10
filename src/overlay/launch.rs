//! Flight Control: die nächsten Raketenstarts aus der Launch Library 2 (The Space Devs).
//!
//! Die einzige Stelle in Veyra mit Netzwerkzugriff – und nur, solange das Widget aktiv ist.
//! Ein Hintergrund-Thread fragt alle 15 Minuten die Liste der nächsten Starts ab (kurz vor einem
//! Start alle 6 Minuten), also deutlich unter der Grenze von 15 Abfragen pro Stunde ohne API-Schlüssel.
//! Für die nächsten zwei Starts kommen Raketenfoto und Mission-Patch dazu – nur von der Bildablage
//! der Launch Library, nur JPEG/PNG, zwischengespeichert in `%APPDATA%\Veyra\cache`.

use super::time::parse_iso_utc;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const HOST: &str = "ll.thespacedevs.com";
const LIST: &str = "/2.3.0/launches/upcoming/?limit=10&mode=normal&hide_recent_previous=true";
/// Einzige Adresse, von der Bilder geladen werden (Bildablage der Launch Library).
const IMAGE_HOST: &str = "thespacedevs-prod.nyc3.digitaloceanspaces.com";
const REFRESH: Duration = Duration::from_secs(15 * 60);
/// Weniger als eine Stunde bis zu einem Start mit genauer Zeit: öfter nachsehen (Verschiebungen, Holds).
const REFRESH_CLOSE: Duration = Duration::from_secs(6 * 60);
const RETRY: Duration = Duration::from_secs(5 * 60);
const RATE_LIMITED: Duration = Duration::from_secs(30 * 60);
/// So oft wird geprüft, ob für den angezeigten Start noch Bilder fehlen.
const ASSET_CHECK: Duration = Duration::from_secs(5 * 60);
const MAX_JSON: usize = 4 * 1024 * 1024;
const MAX_IMAGE: usize = 8 * 1024 * 1024;
const PHOTO_SIDE: u32 = 1024;
const PATCH_SIDE: u32 = 512;
/// Bilder im Zwischenspeicher, die so lange nicht mehr gebraucht wurden, werden gelöscht.
const CACHE_DAYS: u64 = 14;
/// So lange nach T-0 bleibt ein Start (mit T+) stehen.
pub const AFTER_LIFTOFF: i64 = 20 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub id: String,
    pub rocket: String,
    pub mission: String,
    pub orbit: String,
    /// Startzeit (Unix-Sekunden, UTC).
    pub net: i64,
    /// Startzeit mindestens auf die Stunde genau → Countdown; sonst nur das Datum.
    pub precise: bool,
    /// Startzeit auf die Minute genau → Countdown-Effekte (Terminal Count, Liftoff).
    pub exact: bool,
    /// Kurzform: "Go", "TBC", "TBD", "Hold", "In Flight", "Success" …
    pub status: String,
    pub pad: String,
    pub location: String,
    pub webcast: bool,
    /// Foto (meist die Rakete auf der Rampe oder beim Start).
    pub image: String,
}

/// Dekodiertes Bild (RGBA, vormultipliziert).
pub struct Visual {
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Geteiltes Bild; zwei gelten als gleich, wenn sie von derselben Adresse stammen.
#[derive(Clone)]
pub struct Asset(pub Arc<Visual>);

impl PartialEq for Asset {
    fn eq(&self, other: &Self) -> bool {
        self.0.url == other.0.url
    }
}

impl std::fmt::Debug for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Asset({} {}x{})", self.0.url, self.0.width, self.0.height)
    }
}

#[derive(Default)]
struct State {
    launches: Vec<Launch>,
    /// Letzter Abruf fehlgeschlagen.
    error: bool,
    /// Start-ID → Adresse des Mission-Patches ("" = keiner).
    patches: HashMap<String, String>,
    /// Bild-Adresse → dekodiertes Bild.
    visuals: HashMap<String, Arc<Visual>>,
}

pub struct LaunchView {
    pub launch: Launch,
    pub photo: Option<Asset>,
    pub patch: Option<Asset>,
}

pub enum View {
    /// Noch keine Daten (erster Abruf läuft).
    Waiting,
    /// Keine Daten und der Abruf schlägt fehl.
    NoSignal,
    Launch(Box<LaunchView>),
}

pub struct Feed {
    state: Arc<Mutex<State>>,
    /// Wird die `Feed` verworfen, endet der Thread beim nächsten Aufwachen.
    _stop: Sender<()>,
}

impl Feed {
    /// `dir` = Konfigurationsordner (dort liegen `launches.json` und der Bild-Zwischenspeicher `cache\`).
    pub fn start(dir: PathBuf) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (stop, rx) = channel::<()>();
        let shared = state.clone();
        let spawned = std::thread::Builder::new().name("flight-control".into()).spawn(move || {
            let mut worker = Worker::new(dir, shared);
            let mut next_list = worker.load_cached();
            loop {
                if Instant::now() >= next_list {
                    next_list = Instant::now() + worker.refresh_list();
                }
                worker.ensure_assets();
                let wait = next_list.saturating_duration_since(Instant::now()).min(ASSET_CHECK);
                // Aufwachen nach `wait` – oder sofort beenden, wenn die `Feed` verworfen wurde.
                if !matches!(rx.recv_timeout(wait), Err(RecvTimeoutError::Timeout)) {
                    break;
                }
            }
            log::debug!("Flight Control beendet");
        });
        if let Err(e) = spawned {
            log::error!("Flight Control starten: {e}");
        }
        Self { state, _stop: stop }
    }

    /// Der Start, der gerade angezeigt werden soll – mit Bildern, sofern schon geladen.
    pub fn view(&self, now: i64) -> View {
        let s = self.state.lock().unwrap();
        let next = upcoming(&s.launches, now).next().cloned();
        match next {
            Some(l) => {
                let asset = |url: &str| s.visuals.get(url).map(|v| Asset(v.clone()));
                let patch = s.patches.get(&l.id).and_then(|u| asset(u));
                let photo = asset(&l.image);
                View::Launch(Box::new(LaunchView { launch: l, photo, patch }))
            }
            None if s.error => View::NoSignal,
            None => View::Waiting,
        }
    }
}

/// Starts, die noch bevorstehen (oder gerade erst abgehoben sind), in zeitlicher Reihenfolge.
fn upcoming(list: &[Launch], now: i64) -> impl Iterator<Item = &Launch> {
    list.iter()
        .filter(|l| !matches!(l.status.as_str(), "Success" | "Failure" | "Partial Failure"))
        .filter(move |l| l.net + AFTER_LIFTOFF > now)
}

/// Arbeitet im Hintergrund-Thread: Liste abrufen, Bilder besorgen, Zwischenspeicher pflegen.
struct Worker {
    list_file: PathBuf,
    cache: PathBuf,
    shared: Arc<Mutex<State>>,
    /// Adressen, die zuletzt fehlschlugen (bis zum nächsten Listenabruf nicht erneut versuchen).
    failed: HashSet<String>,
}

impl Worker {
    fn new(dir: PathBuf, shared: Arc<Mutex<State>>) -> Self {
        let cache = dir.join("cache");
        if let Err(e) = std::fs::create_dir_all(&cache) {
            log::warn!("Flight Control: {cache:?} anlegen: {e}");
        }
        Self { list_file: dir.join("launches.json"), cache, shared, failed: HashSet::new() }
    }

    /// Zwischengespeicherte Daten laden; liefert, wann die Liste wieder abgerufen werden soll.
    fn load_cached(&mut self) -> Instant {
        let mut due = Instant::now();
        if let Ok(text) = std::fs::read_to_string(&self.list_file) {
            match parse(&text) {
                Ok(list) => {
                    let age = std::fs::metadata(&self.list_file).and_then(|m| m.modified()).ok().and_then(|m| SystemTime::now().duration_since(m).ok());
                    due += refresh_interval(&list).saturating_sub(age.unwrap_or(Duration::MAX));
                    self.shared.lock().unwrap().launches = list;
                }
                Err(e) => log::warn!("Flight Control: Zwischenspeicher unbrauchbar: {e:#}"),
            }
        }
        if let Ok(text) = std::fs::read_to_string(self.cache.join("patches.json")) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
                self.shared.lock().unwrap().patches = map;
            }
        }
        due
    }

    /// Liste abrufen; liefert die Wartezeit bis zum nächsten Abruf.
    fn refresh_list(&mut self) -> Duration {
        match fetch_list() {
            Ok((text, list)) => {
                log::info!("Flight Control: {} kommende Starts geladen", list.len());
                if let Err(e) = std::fs::write(&self.list_file, text) {
                    log::warn!("Flight Control: {:?} schreiben: {e}", self.list_file);
                }
                let wait = refresh_interval(&list);
                let mut s = self.shared.lock().unwrap();
                s.launches = list;
                s.error = false;
                drop(s);
                self.failed.clear();
                self.clean_cache();
                wait
            }
            Err(e) => {
                log::warn!("Flight Control: {e:#}");
                self.shared.lock().unwrap().error = true;
                if format!("{e}").contains("HTTP 429") {
                    RATE_LIMITED
                } else {
                    RETRY
                }
            }
        }
    }

    /// Patch-Adresse und Bilder der nächsten zwei Starts besorgen; alles andere aus dem Speicher werfen.
    fn ensure_assets(&mut self) {
        let now = super::time::now_unix();
        let wanted: Vec<Launch> = upcoming(&self.shared.lock().unwrap().launches, now).take(2).cloned().collect();
        let mut keep = HashSet::new();
        for l in &wanted {
            let known = self.shared.lock().unwrap().patches.get(&l.id).cloned();
            let patch = match known {
                Some(p) => p,
                None if self.failed.contains(&l.id) => String::new(),
                None => match fetch_patch(&l.id) {
                    Ok(p) => {
                        let mut s = self.shared.lock().unwrap();
                        s.patches.insert(l.id.clone(), p.clone());
                        let json = serde_json::to_string(&s.patches).unwrap_or_default();
                        drop(s);
                        let _ = std::fs::write(self.cache.join("patches.json"), json);
                        p
                    }
                    Err(e) => {
                        log::warn!("Flight Control: Patch für {}: {e:#}", l.mission);
                        self.failed.insert(l.id.clone());
                        String::new()
                    }
                },
            };
            for (url, side) in [(l.image.clone(), PHOTO_SIDE), (patch, PATCH_SIDE)] {
                if url.is_empty() {
                    continue;
                }
                keep.insert(url.clone());
                if self.shared.lock().unwrap().visuals.contains_key(&url) || self.failed.contains(&url) {
                    continue;
                }
                match self.load_visual(&url, side) {
                    Ok(v) => {
                        self.shared.lock().unwrap().visuals.insert(url, Arc::new(v));
                    }
                    Err(e) => {
                        log::warn!("Flight Control: Bild {url}: {e:#}");
                        self.failed.insert(url);
                    }
                }
            }
        }
        self.shared.lock().unwrap().visuals.retain(|url, _| keep.contains(url));
    }

    fn load_visual(&self, url: &str, side: u32) -> Result<Visual> {
        let path = self.cache.join(cache_name(url)?);
        let bytes = match std::fs::read(&path) {
            Ok(b) => {
                // "Zuletzt gebraucht" auffrischen, damit das Aufräumen es nicht löscht.
                let _ = std::fs::File::options().append(true).open(&path).and_then(|f| f.set_modified(SystemTime::now()));
                b
            }
            Err(_) => {
                let b = download(url)?;
                std::fs::write(&path, &b).with_context(|| format!("{path:?} schreiben"))?;
                b
            }
        };
        match crate::platform::sys::decode_image(&bytes, side) {
            Ok((width, height, rgba)) => Ok(Visual { url: url.to_string(), width, height, rgba }),
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                Err(e)
            }
        }
    }

    /// Bilder löschen, die lange nicht gebraucht wurden.
    fn clean_cache(&self) {
        let Ok(dir) = std::fs::read_dir(&self.cache) else { return };
        let limit = Duration::from_secs(CACHE_DAYS * 86_400);
        for e in dir.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|m| m.elapsed().ok()).is_some_and(|age| age > limit);
            if old && e.file_name() != "patches.json" {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn refresh_interval(list: &[Launch]) -> Duration {
    let now = super::time::now_unix();
    let close = upcoming(list, now).next().is_some_and(|l| l.exact && l.net - now < 3600);
    if close {
        REFRESH_CLOSE
    } else {
        REFRESH
    }
}

fn get_json(path: &str) -> Result<String> {
    let (status, body) = crate::platform::sys::https_get(HOST, path, MAX_JSON)?;
    if status != 200 {
        bail!("{HOST} antwortet mit HTTP {status}");
    }
    String::from_utf8(body).context("Antwort ist kein UTF-8")
}

fn fetch_list() -> Result<(String, Vec<Launch>)> {
    let text = get_json(LIST)?;
    let list = parse(&text)?;
    Ok((text, list))
}

/// Mission-Patch eines Starts (nur in der ausführlichen Ansicht enthalten). "" = keiner.
fn fetch_patch(id: &str) -> Result<String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        bail!("ungültige Start-ID {id:?}");
    }
    let text = get_json(&format!("/2.3.0/launches/{id}/?mode=detailed"))?;
    let v: Value = serde_json::from_str(&text).context("Antwort ist kein JSON")?;
    Ok(patch_url(&v))
}

fn patch_url(v: &Value) -> String {
    let Some(list) = v.get("mission_patches").and_then(Value::as_array) else { return String::new() };
    list.iter()
        .filter_map(|p| Some((p.get("priority").and_then(Value::as_i64).unwrap_or(0), p.get("image_url")?.as_str()?)))
        .filter(|(_, u)| image_path(u).is_ok())
        .min_by_key(|(prio, _)| *prio)
        .map(|(_, u)| u.to_string())
        .unwrap_or_default()
}

/// Nur Bilder aus der Bildablage der Launch Library, nur per HTTPS.
fn image_path(url: &str) -> Result<&str> {
    let rest = url.strip_prefix("https://").context("nur HTTPS")?;
    let (host, path) = rest.split_once('/').context("Adresse ohne Pfad")?;
    if host != IMAGE_HOST {
        bail!("Bilder nur von {IMAGE_HOST}");
    }
    if !path.starts_with("media/") || path.contains("..") || path.contains(['?', '#', '\\']) {
        bail!("unerwarteter Pfad");
    }
    Ok(&url[8 + host.len()..])
}

/// Dateiname im Zwischenspeicher: letzter Pfadteil, nur unbedenkliche Zeichen.
fn cache_name(url: &str) -> Result<String> {
    let path = image_path(url)?;
    let name: String = path.rsplit('/').next().unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).collect();
    if name.is_empty() || name.starts_with('.') || name == "patches.json" {
        bail!("ungültiger Dateiname");
    }
    Ok(name)
}

fn download(url: &str) -> Result<Vec<u8>> {
    let path = image_path(url)?;
    let (status, body) = crate::platform::sys::https_get(IMAGE_HOST, path, MAX_IMAGE)?;
    if status != 200 {
        bail!("HTTP {status}");
    }
    Ok(body)
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
    let image = str_at(v, &["image", "image_url"]);
    Some(Launch {
        id: str_at(v, &["id"]).to_string(),
        rocket: rocket.to_string(),
        mission: mission.to_string(),
        orbit: if orbit == "N/A" { String::new() } else { orbit.to_string() },
        net,
        precise: matches!(precision, "" | "Second" | "Minute" | "Hour"),
        exact: matches!(precision, "" | "Second" | "Minute"),
        status: str_at(v, &["status", "abbrev"]).to_string(),
        pad: short_pad(str_at(v, &["pad", "name"])),
        location: str_at(v, &["pad", "location", "name"]).split(',').next().unwrap_or("").trim().to_string(),
        webcast: v.get("webcast_live").and_then(Value::as_bool).unwrap_or(false),
        image: if image_path(image).is_ok() { image.to_string() } else { String::new() },
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

/// Für Tests und die Vorschau: Bild direkt aus einer Datei laden.
#[allow(dead_code)]
pub fn visual_from_file(path: &Path, url: &str, side: u32) -> Result<Visual> {
    let bytes = std::fs::read(path)?;
    let (width, height, rgba) = crate::platform::sys::decode_image(&bytes, side)?;
    Ok(Visual { url: url.to_string(), width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"count":2,"results":[
        {"id":"6aa7b90b-e5ed","name":"Falcon 9 Block 5 | USSF-627","net":"2026-11-12T02:35:00Z",
         "status":{"abbrev":"Go"},"net_precision":{"name":"Minute"},"webcast_live":false,
         "image":{"image_url":"https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/images/falcon_9.jpg"},
         "rocket":{"configuration":{"name":"Falcon 9"}},
         "mission":{"name":"USSF-627","orbit":{"abbrev":"LEO"}},
         "pad":{"name":"Space Launch Complex 40","location":{"name":"Cape Canaveral SFS, FL, USA"}}},
        {"id":"d66590ab","name":"Long March 2D | Unknown Payload","net":"2026-11-11T11:40:00Z",
         "status":{"abbrev":"TBC"},"net_precision":{"name":"Day"},
         "image":{"image_url":"https://evil.example.com/media/x.jpg"},
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
        assert!(!lm.precise && !lm.exact && lm.orbit.is_empty());
        assert!(lm.image.is_empty(), "Bild von fremder Adresse wird verworfen");
        let f9 = &list[1];
        assert_eq!((f9.pad.as_str(), f9.location.as_str(), f9.orbit.as_str()), ("SLC-40", "Cape Canaveral SFS", "LEO"));
        assert!(f9.precise && f9.exact);
        assert!(f9.image.ends_with("falcon_9.jpg"));
    }

    #[test]
    fn picks_next_launch() {
        let list = parse(SAMPLE).unwrap();
        let next = |now| upcoming(&list, now).next().map(|l| l.rocket.as_str());
        let t_lm = list[0].net;
        assert_eq!(next(t_lm - 60), Some("Long March 2D"));
        assert_eq!(next(t_lm + 60), Some("Long March 2D"), "kurz nach T-0 noch mit T+ sichtbar");
        assert_eq!(next(t_lm + AFTER_LIFTOFF + 1), Some("Falcon 9"));
        assert_eq!(next(list[1].net + AFTER_LIFTOFF), None);
    }

    #[test]
    fn only_trusted_images() {
        assert!(image_path("https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/images/a.jpg").is_ok());
        for bad in [
            "http://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/images/a.jpg",
            "https://evil.example.com/media/a.jpg",
            "https://thespacedevs-prod.nyc3.digitaloceanspaces.com.evil.com/media/a.jpg",
            "https://thespacedevs-prod.nyc3.digitaloceanspaces.com/other/a.jpg",
            "https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/../../a.jpg",
            "https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/a.jpg?x=1",
        ] {
            assert!(image_path(bad).is_err(), "{bad}");
        }
        assert_eq!(
            cache_name("https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/mission_patch_images/starship2520if_patch.png").unwrap(),
            "starship2520if_patch.png"
        );
    }

    #[test]
    fn picks_patch_with_best_priority() {
        let v: Value = serde_json::from_str(
            r#"{"mission_patches":[
                {"priority":5,"image_url":"https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/mission_patch_images/b.png"},
                {"priority":1,"image_url":"https://thespacedevs-prod.nyc3.digitaloceanspaces.com/media/mission_patch_images/a.png"},
                {"priority":0,"image_url":"https://evil.example.com/media/c.png"}]}"#,
        )
        .unwrap();
        assert!(patch_url(&v).ends_with("/a.png"));
        assert_eq!(patch_url(&serde_json::json!({})), "");
    }
}
