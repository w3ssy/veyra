//! Konfiguration (TOML) – liegt unter %APPDATA%\Veyra\config.toml bzw. ~/.config/veyra/config.toml.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub audio: Audio,
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
        Self { general: General::default(), audio: Audio::default(), wallpapers: vec![Wallpaper::default()] }
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
            std::fs::write(path, DEFAULT_CONFIG).with_context(|| format!("{path:?} schreiben"))?;
            log::info!("Standard-Konfiguration angelegt: {path:?}");
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

/// Kommentierte Vorlage, die beim ersten Start angelegt wird.
pub const DEFAULT_CONFIG: &str = r#"# Veyra – Konfiguration
# Änderungen werden automatisch erkannt (oder Tray-Menü → "Konfiguration neu laden").

[general]
autostart = false            # Mit Windows starten
max_fps = 60                 # Obergrenze Bilder/s (30 spart nochmals Strom)
pause_on_fullscreen = true   # Pause bei Spielen/Vollbild-Videos auf dem Monitor
pause_on_maximized = false   # Pause, wenn ein maximiertes Fenster den Monitor verdeckt
pause_on_battery = true      # Pause im Akkubetrieb / Energiesparmodus
pause_on_lock = true         # Pause bei gesperrtem Bildschirm
gpu = "low"                  # "low" = integrierte GPU (sparsam), "high" = dedizierte GPU
backend = "auto"             # "auto" (= DX12 unter Windows), "dx12" oder "vulkan"

[audio]
enabled = true               # Audio-Reaktion global an/aus
sensitivity = 1.0            # 0.1 – 5.0
smoothing = 0.6              # 0.0 – 0.95

# Ein Block pro Wallpaper. monitor = "all", "primary" oder 1, 2, 3 … (von links nach rechts).
# Ein spezifischer Eintrag (Nummer) hat Vorrang vor "primary", das wiederum vor "all".
[[wallpaper]]
monitor = "all"
video = ''                   # z. B. 'C:\Users\Du\Videos\regen.mp4' (einfache Anführungszeichen!)
fit = "cover"                # "cover", "contain" oder "stretch"
effect = "none"              # "none", "pulse", "glow" oder "chroma" (audio-reaktiv)
intensity = 1.0              # 0.0 – 3.0

# [[wallpaper]]
# monitor = 2
# video = 'D:\Wallpaper\stadt-nacht.mp4'
# effect = "pulse"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_parses() {
        let cfg: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
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
        let s = set_toml_bool(DEFAULT_CONFIG, "general", "autostart", true);
        assert!(s.contains("autostart = true   # Mit Windows starten"));
        let s = set_toml_bool(&s, "audio", "enabled", false);
        let cfg: Config = toml::from_str(&s).unwrap();
        assert!(cfg.general.autostart);
        assert!(!cfg.audio.enabled);
        // fehlende Sektion wird angelegt
        let s = set_toml_bool("", "audio", "enabled", true);
        assert!(toml::from_str::<Config>(&s).unwrap().audio.enabled);
    }

    #[test]
    fn unknown_keys_rejected() {
        assert!(toml::from_str::<Config>("[general]\nfoo = 1\n").is_err());
    }
}
