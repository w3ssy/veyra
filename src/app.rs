//! Die App: verbindet Konfiguration, Fenster, Dekoder, GPU und Audio – und entscheidet,
//! wann überhaupt gerechnet wird. Grundsatz: Nichts tun, wenn es niemand sieht.

use crate::audio::{AudioCapture, Levels, SharedLevels};
use crate::config::{self, Config, Effect, Wallpaper};
use crate::platform::sys::{self, DesktopHost, Event, MenuItem, Monitor, Tray, WallpaperWindow};
use crate::render::{DrawParams, Gpu, Target, VideoTex};
use crate::video::Player;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const IDLE_WAKE: Duration = Duration::from_millis(500);
const SYSTEM_CHECK: Duration = Duration::from_millis(500);
const CONFIG_CHECK: Duration = Duration::from_secs(2);

mod menu {
    pub const PAUSE: u32 = 1;
    pub const AUDIO: u32 = 2;
    pub const OPEN_CONFIG: u32 = 3;
    pub const RELOAD: u32 = 4;
    pub const OPEN_FOLDER: u32 = 5;
    pub const AUTOSTART: u32 = 6;
    pub const QUIT: u32 = 9;
}

struct Slot {
    // Reihenfolge wichtig: Oberfläche vor dem Fenster freigeben.
    target: Target,
    window: WallpaperWindow,
    monitor_index: usize,
    wallpaper: Wallpaper,
    paused: bool,
    dirty: bool,
    last_render: Instant,
}

struct PlayerEntry {
    player: Player,
    tex: Option<VideoTex>,
    fresh: bool,
}

pub struct App {
    cfg: Config,
    cfg_path: PathBuf,
    cfg_mtime: Option<SystemTime>,
    gpu: Gpu,
    // Reihenfolge: Slots (Fenster) vor Tray/Steuerfenster abbauen.
    slots: Vec<Slot>,
    players: HashMap<PathBuf, PlayerEntry>,
    tray: Tray,
    // Muss leben, solange die App läuft (empfängt Tray- und Systemnachrichten).
    _control: sys::ControlWindow,
    monitors: Vec<Monitor>,
    levels: SharedLevels,
    audio: Option<AudioCapture>,
    manual_pause: bool,
    locked: bool,
    sys_state: sys::SystemState,
    start: Instant,
    last_tick: Instant,
    last_sys_check: Instant,
    last_cfg_check: Instant,
    rebuild_at: Option<Instant>,
    quit: bool,
}

pub fn run() -> Result<()> {
    sys::init_process();
    let Some(_instance) = sys::single_instance() else {
        log::info!("Veyra läuft bereits.");
        return Ok(());
    };

    let cfg_path = config::config_path();
    let cfg = Config::load_or_create(&cfg_path)?;
    let gpu = Gpu::new(cfg.general.backend, cfg.general.gpu);

    let control = sys::ControlWindow::new()?;
    let tray = Tray::new(&control);
    let now = Instant::now();
    let mut app = App {
        cfg_mtime: mtime(&cfg_path),
        cfg,
        cfg_path,
        gpu,
        slots: Vec::new(),
        players: HashMap::new(),
        tray,
        _control: control,
        monitors: Vec::new(),
        levels: Default::default(),
        audio: None,
        manual_pause: false,
        locked: false,
        sys_state: Default::default(),
        start: now,
        last_tick: now,
        last_sys_check: now - SYSTEM_CHECK,
        last_cfg_check: now,
        rebuild_at: None,
        quit: false,
    };
    app.apply_autostart();
    app.rebuild();

    while !app.quit {
        if !sys::pump() {
            break;
        }
        for e in sys::take_events() {
            app.handle(e);
        }
        let wake = app.tick();
        sys::wait(wake);
    }

    drop(app);
    sys::refresh_desktop();
    log::info!("Beendet.");
    Ok(())
}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl App {
    fn handle(&mut self, e: Event) {
        match e {
            Event::TrayMenu => self.show_menu(),
            Event::TrayActivate => self.toggle_pause(),
            Event::ShellRestarted => {
                log::info!("Explorer neu gestartet – baue Wallpaper neu auf");
                self.tray.refresh_icon();
                self.tray.add();
                self.schedule_rebuild(Duration::from_millis(1500));
            }
            Event::DisplayChanged => self.schedule_rebuild(Duration::from_millis(800)),
            Event::ThemeChanged => self.tray.refresh_icon(),
            Event::Locked(l) => self.locked = l,
            Event::Quit => self.quit = true,
        }
    }

    fn schedule_rebuild(&mut self, delay: Duration) {
        self.rebuild_at = Some(Instant::now() + delay);
    }

    /// Fenster, Oberflächen, Dekoder und Audio passend zu Konfiguration und Monitoren (neu) anlegen.
    fn rebuild(&mut self) {
        self.slots.clear();
        self.monitors = sys::monitors();
        for (i, m) in self.monitors.iter().enumerate() {
            log::info!("Monitor {}: {} {}x{} @ {},{}{}", i + 1, m.name, m.width, m.height, m.x, m.y, if m.primary { " (primär)" } else { "" });
        }

        let primary: Vec<bool> = self.monitors.iter().map(|m| m.primary).collect();
        let wanted: Vec<(usize, Wallpaper)> =
            self.cfg.assign(&primary).into_iter().enumerate().filter_map(|(i, w)| w.map(|w| (i, w.clone()))).collect();

        // Dekoder nur für tatsächlich genutzte Dateien behalten bzw. starten.
        self.players.retain(|p, _| wanted.iter().any(|(_, w)| &w.video == p));
        for (_, w) in &wanted {
            self.players
                .entry(w.video.clone())
                .or_insert_with(|| PlayerEntry { player: Player::open(&w.video), tex: None, fresh: false });
        }

        if !wanted.is_empty() {
            match DesktopHost::find() {
                Ok(host) => {
                    for (i, w) in wanted {
                        match self.create_slot(&host, i, w) {
                            Ok(s) => self.slots.push(s),
                            Err(e) => log::error!("Monitor {}: {e:?}", i + 1),
                        }
                    }
                }
                Err(e) => {
                    // Passiert z. B. beim Autostart, wenn Explorer noch nicht fertig ist.
                    log::warn!("{e:?} – neuer Versuch in 3 s");
                    self.schedule_rebuild(Duration::from_secs(3));
                }
            }
        }

        // Audio nur mitschneiden, wenn es auch genutzt wird.
        let need_audio = self.cfg.needs_audio();
        if need_audio && self.audio.is_none() {
            self.audio = Some(AudioCapture::start(self.levels.clone(), self.cfg.audio.sensitivity, self.cfg.audio.smoothing));
        } else if !need_audio {
            self.audio = None;
        }
        self.update_tooltip();
    }

    fn create_slot(&mut self, host: &DesktopHost, monitor_index: usize, wallpaper: Wallpaper) -> Result<Slot> {
        let window = WallpaperWindow::create(host, &self.monitors[monitor_index]).context("Fenster anlegen")?;
        let (display, handle) = window.raw_handles();
        // SAFETY: `target` steht in `Slot` vor `window` und wird daher zuerst freigegeben.
        let target = unsafe { self.gpu.create_target(display, handle, window.width, window.height)? };
        Ok(Slot { target, window, monitor_index, wallpaper, paused: false, dirty: true, last_render: Instant::now() - Duration::from_secs(1) })
    }

    fn reload_config(&mut self) {
        match Config::load_or_create(&self.cfg_path) {
            Ok(cfg) => {
                let gpu_changed = cfg.general.gpu != self.cfg.general.gpu || cfg.general.backend != self.cfg.general.backend;
                let audio_changed = cfg.audio != self.cfg.audio;
                self.cfg = cfg;
                if gpu_changed {
                    log::warn!("GPU-/Backend-Änderung wird erst nach einem Neustart von Veyra wirksam");
                }
                if audio_changed {
                    self.audio = None; // mit neuen Parametern neu starten
                }
                self.apply_autostart();
                log::info!("Konfiguration neu geladen");
                self.rebuild();
            }
            Err(e) => {
                log::error!("Konfiguration fehlerhaft, behalte die alte: {e:?}");
                self.tray.set_tooltip("Veyra – Fehler in config.toml (siehe Log)");
            }
        }
    }

    fn apply_autostart(&self) {
        if self.cfg.general.autostart != sys::autostart_enabled() {
            if let Err(e) = sys::set_autostart(self.cfg.general.autostart) {
                log::error!("{e:?}");
            }
        }
    }

    /// Schalter aus dem Tray dauerhaft in config.toml schreiben (Kommentare bleiben erhalten).
    fn write_toggle(&mut self, section: &str, key: &str, value: bool) {
        let text = std::fs::read_to_string(&self.cfg_path).unwrap_or_default();
        let new = config::set_toml_bool(&text, section, key, value);
        if let Err(e) = std::fs::write(&self.cfg_path, new) {
            log::error!("config.toml schreiben: {e}");
            return;
        }
        self.cfg_mtime = mtime(&self.cfg_path);
        self.reload_config();
    }

    fn toggle_pause(&mut self) {
        self.manual_pause = !self.manual_pause;
        self.update_tooltip();
    }

    fn update_tooltip(&self) {
        let n = self.slots.len();
        let text = if self.manual_pause {
            "Veyra – pausiert".to_string()
        } else if n == 0 {
            "Veyra – kein Video eingestellt".to_string()
        } else {
            format!("Veyra – {n} Monitor(e) aktiv")
        };
        self.tray.set_tooltip(&text);
    }

    fn show_menu(&mut self) {
        let pause_label = if self.manual_pause { "Fortsetzen" } else { "Pausieren" };
        let id = self.tray.show_menu(&[
            MenuItem::Item { id: menu::PAUSE, label: pause_label, checked: false },
            MenuItem::Item { id: menu::AUDIO, label: "Audio-reaktiv", checked: self.cfg.audio.enabled },
            MenuItem::Separator,
            MenuItem::Item { id: menu::OPEN_CONFIG, label: "Konfiguration bearbeiten…", checked: false },
            MenuItem::Item { id: menu::RELOAD, label: "Konfiguration neu laden", checked: false },
            MenuItem::Item { id: menu::OPEN_FOLDER, label: "Ordner mit Log öffnen", checked: false },
            MenuItem::Separator,
            MenuItem::Item { id: menu::AUTOSTART, label: "Mit Windows starten", checked: sys::autostart_enabled() },
            MenuItem::Separator,
            MenuItem::Item { id: menu::QUIT, label: "Beenden", checked: false },
        ]);
        match id {
            menu::PAUSE => self.toggle_pause(),
            menu::AUDIO => self.write_toggle("audio", "enabled", !self.cfg.audio.enabled),
            menu::OPEN_CONFIG => sys::open_path(&self.cfg_path),
            menu::RELOAD => self.reload_config(),
            menu::OPEN_FOLDER => sys::open_path(&config::config_dir()),
            menu::AUTOSTART => self.write_toggle("general", "autostart", !sys::autostart_enabled()),
            menu::QUIT => self.quit = true,
            _ => {}
        }
    }

    /// Ein Durchlauf der Hauptschleife. Liefert, wie lange danach geschlafen werden darf.
    fn tick(&mut self) -> Duration {
        let now = Instant::now();
        let dt = now - self.last_tick;
        self.last_tick = now;

        if self.rebuild_at.is_some_and(|t| now >= t) {
            self.rebuild_at = None;
            self.rebuild();
        }
        if now - self.last_cfg_check >= CONFIG_CHECK {
            self.last_cfg_check = now;
            let m = mtime(&self.cfg_path);
            if m != self.cfg_mtime {
                self.cfg_mtime = m;
                self.reload_config();
            }
        }
        if now - self.last_sys_check >= SYSTEM_CHECK {
            self.last_sys_check = now;
            self.update_pause_state();
        }

        // Videos weiterlaufen lassen, deren Monitor sichtbar ist.
        for (path, entry) in self.players.iter_mut() {
            entry.fresh = false;
            let active = self.slots.iter().any(|s| !s.paused && &s.wallpaper.video == path);
            if !active {
                continue;
            }
            entry.player.advance(dt);
            if let Some(frame) = entry.player.take_due() {
                self.gpu.upload(&mut entry.tex, &frame);
                entry.player.recycle(frame);
                entry.fresh = true;
            }
        }

        let audio_on = self.audio.is_some();
        let levels = if audio_on { self.levels.lock().map(|l| *l).unwrap_or_default() } else { Levels::default() };
        let interval = Duration::from_secs_f64(1.0 / self.cfg.general.max_fps as f64);
        let time = (now - self.start).as_secs_f32();
        let mut anything_active = false;

        for slot in &mut self.slots {
            if slot.paused {
                continue;
            }
            anything_active = true;
            let Some(entry) = self.players.get(&slot.wallpaper.video) else { continue };
            let Some(tex) = entry.tex.as_ref() else { continue };
            slot.dirty |= entry.fresh;
            // Bei Stille nicht neu zeichnen – sonst liefe die GPU umsonst mit max_fps.
            let audible = levels.bass + levels.mid + levels.high + levels.level > 0.004;
            let animated = audio_on && audible && slot.wallpaper.effect != Effect::None;
            if (slot.dirty || animated) && now - slot.last_render >= interval.mul_f32(0.9) {
                let params = DrawParams {
                    fit: slot.wallpaper.fit,
                    effect: if audio_on { slot.wallpaper.effect } else { Effect::None },
                    intensity: slot.wallpaper.intensity,
                    levels,
                    time,
                };
                self.gpu.render(&mut slot.target, tex, &params);
                slot.last_render = now;
                slot.dirty = false;
            }
        }

        if anything_active {
            interval.min(Duration::from_millis(16))
        } else {
            IDLE_WAKE
        }
    }

    fn update_pause_state(&mut self) {
        let own: Vec<isize> = self.slots.iter().map(|s| s.window.id()).collect();
        self.sys_state = sys::system_state(&self.monitors, &own);
        let g = &self.cfg.general;
        let st = &self.sys_state;
        let global = self.manual_pause
            || (g.pause_on_lock && self.locked)
            || (g.pause_on_battery && st.on_battery)
            || (g.pause_on_fullscreen && st.exclusive_fullscreen);

        for slot in &mut self.slots {
            let covered = st.fg_monitor == Some(slot.monitor_index)
                && ((g.pause_on_fullscreen && st.fg_fullscreen) || (g.pause_on_maximized && st.fg_maximized));
            let paused = global || covered;
            if paused != slot.paused {
                log::debug!("Monitor {}: {}", slot.monitor_index + 1, if paused { "pausiert" } else { "läuft" });
                slot.paused = paused;
                slot.dirty = true;
            }
        }

        // Audio-Mitschnitt anhalten, wenn kein sichtbarer Monitor einen Effekt nutzt.
        let audio_needed = self.cfg.needs_audio() && self.slots.iter().any(|s| !s.paused && s.wallpaper.effect != Effect::None);
        if audio_needed && self.audio.is_none() {
            self.audio = Some(AudioCapture::start(self.levels.clone(), self.cfg.audio.sensitivity, self.cfg.audio.smoothing));
        } else if !audio_needed && self.audio.is_some() {
            self.audio = None;
        }
    }
}
