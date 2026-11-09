//! Konfiguration (TOML) – liegt unter %APPDATA%\Veyra\config.toml bzw. ~/.config/veyra/config.toml.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub audio: Audio,
    pub library: Library,
    #[serde(rename = "wallpaper")]
    pub wallpapers: Vec<Wallpaper>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// Mit Windows starten (Registry-Eintrag unter HKCU\...\Run).
    pub autostart: bool,
    /// Obergrenze für Bilder pro Sekunde (Video läuft trotzdem in Echtzeit).
    pub max_fps: u32,
    /// Pausieren, wenn eine Vollbild-Anwendung (Spiel, Video) auf dem Monitor läuft.
    pub pause_on_fullscreen: bool,
    /// Pausieren, wenn das Vordergrundfenster auf dem Monitor maximiert ist.
    pub pause_on_maximized: bool,
    /// Pausieren im Akkubetrieb oder bei aktivem Energiesparmodus.
    pub pause_on_battery: bool,
    /// Pausieren, wenn die Sitzung gesperrt ist.
    pub pause_on_lock: bool,
    /// Pausieren, wenn der Bildschirm ausgeschaltet ist (Energiesparen/Standby des Monitors).
    pub pause_on_display_off: bool,
    /// "low" (iGPU, spart Strom) oder "high" (dedizierte GPU).
    pub gpu: GpuPreference,
    /// Grafik-Backend: "auto", "dx12" oder "vulkan".
    pub backend: Backend,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GpuPreference {
    Low,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Auto,
    Dx12,
    Vulkan,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// Systemsound mitschneiden (nur lokal, nichts wird gespeichert oder versendet).
    pub enabled: bool,
    /// Verstärkung der Reaktion (0.1 – 5.0).
    pub sensitivity: f32,
    /// Glättung (0.0 = nervös, 0.95 = sehr träge).
    pub smoothing: f32,
}

/// Video-Bibliothek: Ordner, dessen Videos das Einstellungsfenster zur Auswahl anbietet.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Library {
    /// Leer = der Windows-Ordner „Videos“.
    pub folder: PathBuf,
}

/// Dateiendungen, die als Video gelten (Bibliothek, Drag & Drop).
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv"];

pub fn is_video(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| VIDEO_EXTENSIONS.iter().any(|v| v.eq_ignore_ascii_case(e)))
}

impl Library {
    pub fn effective_folder(&self) -> PathBuf {
        if !self.folder.as_os_str().is_empty() {
            return self.folder.clone();
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
        home.join("Videos")
    }

    /// Videos im Bibliotheksordner (nicht rekursiv), alphabetisch.
    pub fn videos(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.effective_folder())
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file() && is_video(p)).collect())
            .unwrap_or_default();
        v.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
        v
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Wallpaper {
    /// "all", "primary" oder Monitor-Nummer ab 1 (von links nach rechts).
    pub monitor: MonitorSel,
    /// Pfad zur Videodatei (MP4/WebM/MKV …). Leer = Monitor bleibt beim normalen Hintergrund.
    pub video: PathBuf,
    /// "cover" (füllen, beschneiden), "contain" (einpassen) oder "stretch".
    pub fit: Fit,
    /// Audio-Effekt: "none", "pulse", "glow" oder "chroma".
    pub effect: Effect,
    /// Stärke des Effekts (0.0 – 3.0).
    pub intensity: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorSel {
    All,
    Primary,
    Index(usize),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    Cover,
    Contain,
    Stretch,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    None,
    Pulse,
    Glow,
    Chroma,
}

impl Default for General {
    fn default() -> Self {
        Self {
            autostart: false,
            max_fps: 60,
            pause_on_fullscreen: true,
            pause_on_maximized: false,
            pause_on_battery: true,
            pause_on_lock: true,
            pause_on_display_off: true,
            gpu: GpuPreference::Low,
            backend: Backend::Auto,
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self { enabled: true, sensitivity: 1.0, smoothing: 0.6 }
    }
}

impl Default for Wallpaper {
    fn default() -> Self {
        Self {
            monitor: MonitorSel::All,
            video: PathBuf::new(),
            fit: Fit::Cover,
            effect: Effect::None,
            intensity: 1.0,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { general: General::default(), audio: Audio::default(), library: Library::default(), wallpapers: vec![Wallpaper::default()] }
    }
}

// MonitorSel als String oder Zahl (de)serialisieren.
impl Serialize for MonitorSel {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MonitorSel::All => s.serialize_str("all"),
            MonitorSel::Primary => s.serialize_str("primary"),
            MonitorSel::Index(i) => s.serialize_u64(*i as u64),
        }
    }
}

impl<'de> Deserialize<'de> for MonitorSel {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u64),
            S(String),
        }
        match Raw::deserialize(d)? {
            Raw::N(0) => Err(serde::de::Error::custom("Monitor-Nummern beginnen bei 1")),
            Raw::N(n) => Ok(MonitorSel::Index(n as usize)),
            Raw::S(s) => match s.to_ascii_lowercase().as_str() {
                "all" | "alle" => Ok(MonitorSel::All),
                "primary" | "primär" | "haupt" => Ok(MonitorSel::Primary),
                other => other
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .map(MonitorSel::Index)
                    .ok_or_else(|| serde::de::Error::custom(format!("unbekannter Monitor: {other}"))),
            },
        }
    }
}

impl Config {
    /// Werte in sinnvolle Grenzen bringen.
    pub fn sanitize(&mut self) {
        self.general.max_fps = self.general.max_fps.clamp(1, 240);
        self.audio.sensitivity = self.audio.sensitivity.clamp(0.1, 5.0);
        self.audio.smoothing = self.audio.smoothing.clamp(0.0, 0.95);
        for w in &mut self.wallpapers {
            w.intensity = w.intensity.clamp(0.0, 3.0);
        }
    }

    /// Wird irgendein Audio-Effekt tatsächlich genutzt?
    pub fn needs_audio(&self) -> bool {
        self.audio.enabled
            && self.wallpapers.iter().any(|w| w.effect != Effect::None && !w.video.as_os_str().is_empty())
    }

    /// Welcher Wallpaper-Eintrag gilt für welchen Monitor? `primary[i]` = ist Monitor i der Hauptmonitor.
    /// Vorrang: Nummer > "primary" > "all". Einträge ohne Video lassen den Monitor frei.
    pub fn assign(&self, primary: &[bool]) -> Vec<Option<&Wallpaper>> {
        primary
            .iter()
            .enumerate()
            .map(|(i, &is_primary)| {
                let rank = |w: &Wallpaper| match w.monitor {
                    MonitorSel::Index(n) if n == i + 1 => Some(3),
                    MonitorSel::Primary if is_primary => Some(2),
                    MonitorSel::All => Some(1),
                    _ => None,
                };
                self.wallpapers
                    .iter()
                    .filter_map(|w| rank(w).map(|r| (r, w)))
                    .max_by_key(|(r, _)| *r)
                    .map(|(_, w)| w)
                    .filter(|w| !w.video.as_os_str().is_empty())
            })
            .collect()
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("Ordner {dir:?} anlegen"))?;
            }
            let legacy = legacy_config_path();
            if legacy.is_file() && std::fs::copy(&legacy, path).is_ok() {
                log::info!("Konfiguration aus {legacy:?} übernommen");
            } else {
                std::fs::write(path, render(&Config::default())).with_context(|| format!("{path:?} schreiben"))?;
                log::info!("Standard-Konfiguration angelegt: {path:?}");
            }
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("{path:?} lesen"))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("{path:?} ist kein gültiges TOML"))?;
        cfg.sanitize();
        Ok(cfg)
    }
}

pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.unwrap_or_else(|| PathBuf::from(".")).join(if cfg!(windows) { "Veyra" } else { "veyra" })
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Konfiguration aus der Zeit, als Veyra noch „Kulisse“ hieß – wird beim ersten Start übernommen.
fn legacy_config_path() -> PathBuf {
    let dir = config_dir();
    dir.parent().unwrap_or(&dir).join(if cfg!(windows) { "Kulisse" } else { "kulisse" }).join("config.toml")
}

/// Konfiguration speichern (kommentiert, siehe [`render`]). Erst in eine temporäre Datei, dann umbenennen –
/// so liest die laufende App nie eine halb geschriebene Datei.
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, render(cfg)).with_context(|| format!("{tmp:?} schreiben"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("{path:?} ersetzen"))?;
    Ok(())
}

/// Einen Bool-Wert in der Datei ändern, ohne Kommentare und Formatierung zu verlieren
/// (für die Schalter im Tray-Menü). Fehlt der Schlüssel, wird er in der Sektion ergänzt.
pub fn set_toml_bool(text: &str, section: &str, key: &str, value: bool) -> String {
    let header = format!("[{section}]");
    let mut out = Vec::new();
    let mut in_section = false;
    let mut done = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            if in_section && !done {
                out.push(format!("{key} = {value}"));
                done = true;
            }
            in_section = t.trim_end().starts_with(&header) && !t.starts_with("[[");
        }
        if in_section && !done {
            if let Some(rest) = t.strip_prefix(key) {
                if rest.trim_start().starts_with('=') {
                    let comment = rest.find('#').map(|i| format!("   {}", &rest[i..])).unwrap_or_default();
                    out.push(format!("{key} = {value}{comment}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !done {
        if !in_section {
            out.push(String::new());
            out.push(header);
        }
        out.push(format!("{key} = {value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

impl GpuPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            GpuPreference::Low => "low",
            GpuPreference::High => "high",
        }
    }
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Auto => "auto",
            Backend::Dx12 => "dx12",
            Backend::Vulkan => "vulkan",
        }
    }
}

impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Cover, Fit::Contain, Fit::Stretch];
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Cover => "cover",
            Fit::Contain => "contain",
            Fit::Stretch => "stretch",
        }
    }
}

impl Effect {
    pub const ALL: [Effect; 4] = [Effect::None, Effect::Pulse, Effect::Glow, Effect::Chroma];
    pub fn as_str(self) -> &'static str {
        match self {
            Effect::None => "none",
            Effect::Pulse => "pulse",
            Effect::Glow => "glow",
            Effect::Chroma => "chroma",
        }
    }
}

/// Pfad als TOML-String: einfache Anführungszeichen (keine Backslash-Verdopplung), wenn möglich.
fn toml_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains('\'') || s.contains('\n') {
        toml::Value::String(s.into_owned()).to_string()
    } else {
        format!("'{s}'")
    }
}

/// Konfiguration als kommentiertes TOML (Vorlage beim ersten Start, Speichern aus dem Einstellungsfenster).
pub fn render(cfg: &Config) -> String {
    let mut o = String::new();
    let mut line = |kv: String, comment: &str| {
        if comment.is_empty() {
            o.push_str(&kv);
        } else {
            o.push_str(&format!("{kv:<28} # {comment}"));
        }
        o.push('\n');
    };
    let g = &cfg.general;
    let a = &cfg.audio;
    line("# Veyra – Konfiguration".into(), "");
    line("# Änderungen werden automatisch übernommen. Bequemer: Tray-Menü → \"Einstellungen…\".".into(), "");
    line(String::new(), "");
    line("[general]".into(), "");
    line(format!("autostart = {}", g.autostart), "Mit Windows starten");
    line(format!("max_fps = {}", g.max_fps), "Obergrenze Bilder/s (30 spart nochmals Strom)");
    line(format!("pause_on_fullscreen = {}", g.pause_on_fullscreen), "Pause bei Spielen/Vollbild-Videos auf dem Monitor");
    line(format!("pause_on_maximized = {}", g.pause_on_maximized), "Pause, wenn ein maximiertes Fenster den Monitor verdeckt");
    line(format!("pause_on_battery = {}", g.pause_on_battery), "Pause im Akkubetrieb / Energiesparmodus");
    line(format!("pause_on_lock = {}", g.pause_on_lock), "Pause bei gesperrtem Bildschirm");
    line(format!("pause_on_display_off = {}", g.pause_on_display_off), "Pause, wenn der Bildschirm aus ist");
    line(format!("gpu = \"{}\"", g.gpu.as_str()), "\"low\" = integrierte GPU (sparsam), \"high\" = dedizierte GPU");
    line(format!("backend = \"{}\"", g.backend.as_str()), "\"auto\" (= DX12 unter Windows), \"dx12\" oder \"vulkan\"");
    line(String::new(), "");
    line("[audio]".into(), "");
    line(format!("enabled = {}", a.enabled), "Audio-Reaktion global an/aus");
    line(format!("sensitivity = {:?}", a.sensitivity), "0.1 – 5.0");
    line(format!("smoothing = {:?}", a.smoothing), "0.0 – 0.95");
    line(String::new(), "");
    line("[library]".into(), "");
    line(format!("folder = {}", toml_path(&cfg.library.folder)), "Ordner mit Videos zur Auswahl (leer = \"Videos\")");
    line(String::new(), "");
    line("# Ein Block pro Wallpaper. monitor = \"all\", \"primary\" oder 1, 2, 3 … (von links nach rechts).".into(), "");
    line("# Ein spezifischer Eintrag (Nummer) hat Vorrang vor \"primary\", das wiederum vor \"all\".".into(), "");
    line("# video = '' lässt den Monitor beim normalen Windows-Hintergrund.".into(), "");
    for w in &cfg.wallpapers {
        let monitor = match &w.monitor {
            MonitorSel::All => "\"all\"".to_string(),
            MonitorSel::Primary => "\"primary\"".to_string(),
            MonitorSel::Index(i) => i.to_string(),
        };
        line("[[wallpaper]]".into(), "");
        line(format!("monitor = {monitor}"), "");
        line(format!("video = {}", toml_path(&w.video)), "");
        line(format!("fit = \"{}\"", w.fit.as_str()), "\"cover\", \"contain\" oder \"stretch\"");
        line(format!("effect = \"{}\"", w.effect.as_str()), "\"none\", \"pulse\", \"glow\" oder \"chroma\" (audio-reaktiv)");
        line(format!("intensity = {:?}", w.intensity), "0.0 – 3.0");
        line(String::new(), "");
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_parses() {
        let cfg: Config = toml::from_str(&render(&Config::default())).unwrap();
        assert_eq!(cfg.wallpapers.len(), 1);
        assert_eq!(cfg.wallpapers[0].monitor, MonitorSel::All);
        assert_eq!(cfg.general.max_fps, 60);
    }

    #[test]
    fn monitor_selectors() {
        let cfg: Config = toml::from_str(
            "[[wallpaper]]\nmonitor = 2\nvideo='a.mp4'\neffect='pulse'\n[[wallpaper]]\nmonitor='primary'\n",
        )
        .unwrap();
        assert_eq!(cfg.wallpapers[0].monitor, MonitorSel::Index(2));
        assert_eq!(cfg.wallpapers[1].monitor, MonitorSel::Primary);
        assert!(cfg.needs_audio());
        assert!(toml::from_str::<Config>("[[wallpaper]]\nmonitor = 0\n").is_err());
    }

    #[test]
    fn assignment_priority() {
        let cfg: Config = toml::from_str(
            "[[wallpaper]]\nmonitor='all'\nvideo='a.mp4'\n\
             [[wallpaper]]\nmonitor='primary'\nvideo='p.mp4'\n\
             [[wallpaper]]\nmonitor=3\nvideo=''\n",
        )
        .unwrap();
        let a = cfg.assign(&[false, true, false]);
        assert_eq!(a[0].unwrap().video.to_str(), Some("a.mp4"));
        assert_eq!(a[1].unwrap().video.to_str(), Some("p.mp4"));
        assert!(a[2].is_none(), "leeres Video lässt Monitor 3 frei");
    }

    #[test]
    fn toggle_keeps_comments() {
        let s = set_toml_bool(&render(&Config::default()), "general", "autostart", true);
        assert!(s.contains("autostart = true") && s.contains("# Mit Windows starten"));
        let s = set_toml_bool(&s, "audio", "enabled", false);
        let cfg: Config = toml::from_str(&s).unwrap();
        assert!(cfg.general.autostart);
        assert!(!cfg.audio.enabled);
        // fehlende Sektion wird angelegt
        let s = set_toml_bool("", "audio", "enabled", true);
        assert!(toml::from_str::<Config>(&s).unwrap().audio.enabled);
    }

    #[test]
    fn render_round_trip() {
        let mut cfg = Config::default();
        cfg.general.autostart = true;
        cfg.audio.smoothing = 0.75;
        cfg.library.folder = PathBuf::from(r"D:\Wall'paper");
        cfg.wallpapers = vec![
            Wallpaper { video: PathBuf::from(r"C:\Videos\a b.mp4"), ..Default::default() },
            Wallpaper { monitor: MonitorSel::Index(2), video: PathBuf::new(), fit: Fit::Contain, effect: Effect::Glow, intensity: 2.5 },
            Wallpaper { monitor: MonitorSel::Primary, ..Default::default() },
        ];
        let back: Config = toml::from_str(&render(&cfg)).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn legacy_config_without_new_keys_loads() {
        let cfg: Config = toml::from_str("[general]\nautostart = true\n[[wallpaper]]\nvideo = 'a.mp4'\n").unwrap();
        assert!(cfg.general.pause_on_display_off);
        assert!(cfg.library.folder.as_os_str().is_empty());
    }

    #[test]
    fn video_extensions() {
        assert!(is_video(Path::new("a.MP4")));
        assert!(!is_video(Path::new("a.txt")));
        assert!(!is_video(Path::new("mp4")));
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(toml::from_str::<Config>("[general]\nfoo = 1\n").is_err());
    }
}
