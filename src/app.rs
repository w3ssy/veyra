//! Die App: verbindet Konfiguration, Fenster, Dekoder, GPU und Audio – und entscheidet,
//! wann überhaupt gerechnet wird. Grundsatz: Nichts tun, wenn es niemand sieht.

use crate::audio::{AudioCapture, Levels, SharedLevels};
use crate::config::{self, Config, Effect, Position, Wallpaper};
use crate::overlay::{self, text::Line};
use crate::platform::sys::{self, DesktopHost, Event, HostState, MenuItem, Monitor, Tray, WallpaperWindow};
use crate::render::{DrawParams, Gpu, Overlay, Target, VideoTex};
use crate::video::{DecodeOptions, Player};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const IDLE_WAKE: Duration = Duration::from_millis(500);
const SYSTEM_CHECK: Duration = Duration::from_millis(500);
const CONFIG_CHECK: Duration = Duration::from_secs(1);

mod menu {
    pub const SETTINGS: u32 = 10;
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
    /// Widgets auf diesem Monitor, eins pro belegter Position.
    panels: Vec<Panel>,
}

struct Panel {
    pos: Position,
    /// Inhalt, aus dem das Bild gerastert wurde – unverändert → nichts zu tun.
    lines: Vec<Line>,
    overlay: Overlay,
}

struct PlayerEntry {
    player: Player,
    opts: DecodeOptions,
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
    /// Desktop-Ebene, in die die aktuellen Fenster eingebettet sind (für den Wächter).
    host: Option<DesktopHost>,
    tray: Tray,
    // Muss leben, solange die App läuft (empfängt Tray- und Systemnachrichten).
    _control: sys::ControlWindow,
    monitors: Vec<Monitor>,
    levels: SharedLevels,
    audio: Option<AudioCapture>,
    widgets: overlay::Widgets,
    /// Sekunde (Unix-Zeit), für die die Widgets zuletzt bestimmt wurden.
    widget_second: i64,
    manual_pause: bool,
    locked: bool,
    display_off: bool,
    /// Zero-Copy erlaubt (wird nach einem Fehler dauerhaft abgeschaltet → Weg über den Arbeitsspeicher).
    zero_copy: bool,
    sys_state: sys::SystemState,
    start: Instant,
    last_tick: Instant,
    last_sys_check: Instant,
    last_cfg_check: Instant,
    rebuild_at: Option<Instant>,
    quit: bool,
}

pub fn run(autostart: bool) -> Result<()> {
    sys::init_process();
    sys::set_app_id();
    let Some(_instance) = sys::single_instance() else {
        // Erneut gestartet (Verknüpfung, angeheftete Kachel …): Einstellungen zeigen statt still zu beenden.
        if !autostart {
            log::info!("Veyra läuft bereits – öffne die Einstellungen");
            sys::open_settings();
        }
        return Ok(());
    };
    if autostart {
        sys::wait_for_shell(Duration::from_secs(60));
    }

    let cfg_path = config::config_path();
    let cfg = Config::load_or_create(&cfg_path)?;
    let gpu = Gpu::new(cfg.general.backend, cfg.general.gpu);
    let mut widgets = overlay::Widgets::new();
    widgets.configure(&cfg.widgets);

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
        host: None,
        tray,
        _control: control,
        monitors: Vec::new(),
        levels: Default::default(),
        audio: None,
        widgets,
        widget_second: 0,
        manual_pause: false,
        locked: false,
        display_off: false,
        zero_copy: true,
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
            Event::DisplayPower(on) => {
                log::debug!("Bildschirm {}", if on { "an" } else { "aus" });
                self.display_off = !on;
                self.last_sys_check = Instant::now() - SYSTEM_CHECK; // sofort neu bewerten
            }
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
        self.host = None;
        self.monitors = sys::monitors();
        for (i, m) in self.monitors.iter().enumerate() {
            log::info!("Monitor {}: {} {}x{} @ {},{}{}", i + 1, m.name, m.width, m.height, m.x, m.y, if m.primary { " (primär)" } else { "" });
        }

        let wanted = self.wanted();

        if !wanted.is_empty() {
            match DesktopHost::find() {
                Ok(host) => {
                    for (i, w) in wanted.clone() {
                        match self.create_slot(&host, i, w) {
                            Ok(s) => self.slots.push(s),
                            Err(e) => log::error!("Monitor {}: {e:?}", i + 1),
                        }
                    }
                    self.host = Some(host);
                }
                Err(e) => {
                    // Passiert z. B. beim Autostart, wenn Explorer noch nicht fertig ist.
                    log::warn!("{e:?} – neuer Versuch in 3 s");
                    self.schedule_rebuild(Duration::from_secs(3));
                }
            }
        }

        // Dekoder pro Datei: Zielgröße = größter Monitor, der sie zeigt. Erst nach den Fenstern anlegen –
        // dann steht die Grafikkarte des Renderers fest, und der Dekoder kann per Zero-Copy liefern.
        let gpu_luid = if self.zero_copy { self.gpu.zero_copy_luid() } else { None };
        let mut wanted_opts: HashMap<PathBuf, DecodeOptions> = HashMap::new();
        for (i, w) in &wanted {
            let m = &self.monitors[*i];
            let o = wanted_opts.entry(w.video.clone()).or_insert(DecodeOptions {
                max_size: Some((0, 0)),
                low_power: self.cfg.general.gpu == crate::config::GpuPreference::Low,
                gpu_luid,
            });
            let (mw, mh) = o.max_size.unwrap_or_default();
            o.max_size = Some((mw.max(m.width as u32), mh.max(m.height as u32)));
        }
        // Nur Dekoder behalten, deren Datei und Einstellungen gleich geblieben sind.
        self.players.retain(|p, e| wanted_opts.get(p) == Some(&e.opts));
        for (path, opts) in wanted_opts {
            self.players
                .entry(path.clone())
                .or_insert_with(|| PlayerEntry { player: Player::open(&path, opts), opts, tex: None, fresh: false });
        }

        self.update_audio();
        self.update_tooltip();
    }

    /// Welcher Monitor zeigt welchen Wallpaper-Eintrag (nur Monitore mit Video).
    fn wanted(&self) -> Vec<(usize, Wallpaper)> {
        let primary: Vec<bool> = self.monitors.iter().map(|m| m.primary).collect();
        self.cfg.assign(&primary).into_iter().enumerate().filter_map(|(i, w)| w.map(|w| (i, w.clone()))).collect()
    }

    /// Audio nur mitschneiden, wenn ein sichtbarer Monitor einen Effekt nutzt.
    fn update_audio(&mut self) {
        let needed = self.cfg.needs_audio() && self.slots.iter().any(|s| !s.paused && s.wallpaper.effect != Effect::None);
        if needed && self.audio.is_none() {
            self.audio = Some(AudioCapture::start(self.levels.clone(), self.cfg.audio.sensitivity, self.cfg.audio.smoothing));
        } else if !needed && self.audio.is_some() {
            self.audio = None;
        }
    }

    fn create_slot(&mut self, host: &DesktopHost, monitor_index: usize, wallpaper: Wallpaper) -> Result<Slot> {
        let window = WallpaperWindow::create(host, &self.monitors[monitor_index]).context("Fenster anlegen")?;
        let (display, handle) = window.raw_handles();
        // SAFETY: `target` steht in `Slot` vor `window` und wird daher zuerst freigegeben.
        let target = unsafe { self.gpu.create_target(display, handle, window.width, window.height)? };
        Ok(Slot {
            target,
            window,
            monitor_index,
            wallpaper,
            paused: false,
            dirty: true,
            last_render: Instant::now() - Duration::from_secs(1),
            panels: Vec::new(),
        })
    }

    fn reload_config(&mut self) {
        match Config::load_or_create(&self.cfg_path) {
            Ok(cfg) => {
                if cfg == self.cfg {
                    return;
                }
                let old = std::mem::replace(&mut self.cfg, cfg);
                if old.general.gpu != self.cfg.general.gpu || old.general.backend != self.cfg.general.backend {
                    log::warn!("GPU-/Backend-Änderung wird erst nach einem Neustart von Veyra wirksam");
                }
                if old.audio != self.cfg.audio {
                    self.audio = None; // mit neuen Parametern neu starten
                }
                self.apply_autostart();
                if old.widgets != self.cfg.widgets {
                    self.widgets.configure(&self.cfg.widgets);
                    for s in &mut self.slots {
                        s.panels.clear();
                        s.dirty = true;
                    }
                    self.widget_second = 0; // sofort neu aufbauen
                }
                // Fenster nur neu anlegen, wenn sich ändert, welcher Monitor welches Video zeigt.
                // Darstellung, Effekte, Pausen-Regeln, Autostart … werden direkt übernommen.
                let wanted = self.wanted();
                let same_layout = wanted.len() == self.slots.len()
                    && wanted.iter().zip(&self.slots).all(|((i, w), s)| *i == s.monitor_index && w.video == s.wallpaper.video);
                if same_layout {
                    for ((_, w), slot) in wanted.into_iter().zip(&mut self.slots) {
                        slot.wallpaper = w;
                        slot.dirty = true;
                    }
                    self.last_sys_check = Instant::now() - SYSTEM_CHECK;
                    self.update_audio();
                    self.update_tooltip();
                    log::info!("Konfiguration übernommen");
                } else {
                    log::info!("Konfiguration neu geladen – Wallpaper werden neu aufgebaut");
                    self.rebuild();
                }
            }
            Err(e) => {
                log::error!("Konfiguration fehlerhaft, behalte die alte: {e:?}");
                self.tray.set_tooltip("Veyra – Fehler in config.toml (siehe Log)");
            }
        }
    }

    fn apply_autostart(&self) {
        let want = self.cfg.general.autostart;
        let needs_write = if want { !sys::autostart_current() } else { sys::autostart_enabled() };
        if needs_write {
            match sys::set_autostart(want) {
                Ok(()) => log::info!("Autostart {}", if want { "eingerichtet" } else { "entfernt" }),
                Err(e) => log::error!("{e:?}"),
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
            MenuItem::Item { id: menu::SETTINGS, label: "Einstellungen…", checked: false },
            MenuItem::Separator,
            MenuItem::Item { id: menu::PAUSE, label: pause_label, checked: false },
            MenuItem::Item { id: menu::AUDIO, label: "Audio-reaktiv", checked: self.cfg.audio.enabled },
            MenuItem::Separator,
            MenuItem::Item { id: menu::OPEN_CONFIG, label: "config.toml bearbeiten…", checked: false },
            MenuItem::Item { id: menu::RELOAD, label: "Konfiguration neu laden", checked: false },
            MenuItem::Item { id: menu::OPEN_FOLDER, label: "Ordner mit Log öffnen", checked: false },
            MenuItem::Separator,
            MenuItem::Item { id: menu::AUTOSTART, label: "Mit Windows starten", checked: sys::autostart_enabled() },
            MenuItem::Separator,
            MenuItem::Item { id: menu::QUIT, label: "Beenden", checked: false },
        ]);
        match id {
            menu::SETTINGS => sys::open_settings(),
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
            self.check_desktop();
            self.update_pause_state();
        }

        // Videos weiterlaufen lassen, deren Monitor sichtbar ist.
        let mut next_frame: Option<Duration> = None;
        let mut zero_copy_failed = false;
        for (path, entry) in self.players.iter_mut() {
            entry.fresh = false;
            let active = self.slots.iter().any(|s| !s.paused && &s.wallpaper.video == path);
            if !active {
                continue;
            }
            entry.player.advance(dt);
            if let Some(frame) = entry.player.take_due() {
                match self.gpu.upload(&mut entry.tex, &frame) {
                    Ok(released) => {
                        if let Some(r) = released {
                            entry.player.release(r);
                        }
                        entry.fresh = true;
                    }
                    Err(e) => {
                        log::warn!("Zero-Copy fehlgeschlagen ({e:#}) – Bilder gehen ab jetzt über den Arbeitsspeicher");
                        zero_copy_failed = true;
                    }
                }
                entry.player.recycle(frame);
            }
            if let Some(d) = entry.player.next_due() {
                next_frame = Some(next_frame.map_or(d, |n| n.min(d)));
            }
        }

        if zero_copy_failed && self.zero_copy {
            self.zero_copy = false;
            self.players.clear(); // mit Arbeitsspeicher-Weg neu starten
            self.schedule_rebuild(Duration::ZERO);
        }

        self.update_widgets();

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
            // Großzügige Toleranz: Bilder kommen nie exakt im Takt. Mit 0,9 fiel bei 60-fps-Videos und
            // max_fps = 60 jedes leicht verfrühte Bild weg; mit 0,75 wird max_fps trotzdem eingehalten.
            if (slot.dirty || animated) && now - slot.last_render >= interval.mul_f32(0.75) {
                let params = DrawParams {
                    fit: slot.wallpaper.fit,
                    effect: if audio_on { slot.wallpaper.effect } else { Effect::None },
                    intensity: slot.wallpaper.intensity,
                    levels,
                    time,
                };
                self.gpu.render(&mut slot.target, tex, &params, slot.panels.iter().map(|p| &p.overlay));
                slot.last_render = now;
                slot.dirty = false;
            }
        }

        if anything_active {
            // Genau zum nächsten fälligen Bild aufwachen (höchstens `interval`, für Audio-Effekte);
            // liefert der Dekoder gerade nichts, kurz darauf erneut nachsehen.
            let wake = next_frame.unwrap_or(Duration::from_millis(4)).clamp(Duration::from_millis(1), interval);
            if self.cfg.widgets.any_enabled() {
                // Spätestens zum nächsten Sekundenwechsel (Uhr, Countdown).
                let ms = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.subsec_millis()).unwrap_or(0);
                wake.min(Duration::from_millis((1000 - ms as u64) + 2))
            } else {
                wake
            }
        } else {
            IDLE_WAKE
        }
    }

    /// Widgets höchstens einmal pro Sekunde neu bestimmen; nur geänderte Positionen neu rastern.
    fn update_widgets(&mut self) {
        let cfg = &self.cfg.widgets;
        if !cfg.any_enabled() {
            for s in &mut self.slots {
                if !s.panels.is_empty() {
                    s.panels.clear();
                    s.dirty = true;
                }
            }
            return;
        }
        let now = overlay::time::now_unix();
        if now == self.widget_second || self.slots.iter().all(|s| s.paused) {
            return;
        }
        self.widget_second = now;
        let content = self.widgets.content(cfg, now);
        for slot in &mut self.slots {
            if slot.paused {
                continue;
            }
            let monitor = &self.monitors[slot.monitor_index];
            let before = slot.panels.len();
            if cfg.on_monitor(slot.monitor_index, monitor.primary) {
                slot.panels.retain(|p| content.iter().any(|(pos, _)| *pos == p.pos));
            } else {
                slot.panels.clear();
            }
            slot.dirty |= slot.panels.len() != before;
            if !cfg.on_monitor(slot.monitor_index, monitor.primary) {
                continue;
            }
            let scale = overlay::scale_for(slot.window.height, cfg);
            for (pos, lines) in &content {
                let existing = slot.panels.iter().position(|p| p.pos == *pos);
                if existing.is_some_and(|i| slot.panels[i].lines == *lines) {
                    continue;
                }
                let img = self.widgets.rasterize(lines, *pos, scale);
                let (x, y) = overlay::place(*pos, &img, (slot.window.width, slot.window.height), monitor.work_insets, scale);
                let old = existing.map(|i| slot.panels.remove(i).overlay);
                let overlay = self.gpu.overlay(old, &slot.target, (img.width, img.height), &img.rgba, (x, y));
                slot.panels.push(Panel { pos: *pos, lines: lines.clone(), overlay });
                slot.dirty = true;
            }
        }
    }

    /// Wächter: Explorer baut die Desktop-Ebene gelegentlich um (v. a. kurz nach der Anmeldung).
    fn check_desktop(&mut self) {
        let Some(host) = &self.host else { return };
        if self.rebuild_at.is_some() || self.slots.is_empty() {
            return;
        }
        let ids: Vec<isize> = self.slots.iter().map(|s| s.window.id()).collect();
        match host.check(&ids) {
            HostState::Ok => {}
            HostState::Reordered => {
                log::warn!("Desktop-Reihenfolge verändert – Wallpaper wieder unter die Symbole sortiert");
                for s in &mut self.slots {
                    s.dirty = true;
                }
            }
            HostState::Broken => {
                log::warn!("Desktop-Einbettung verloren – baue Wallpaper neu auf");
                self.schedule_rebuild(Duration::from_millis(500));
            }
        }
    }

    fn update_pause_state(&mut self) {
        let mut own: Vec<isize> = self.slots.iter().map(|s| s.window.id()).collect();
        own.push(self._control.id());
        self.sys_state = sys::system_state(&self.monitors, &own);
        let g = &self.cfg.general;
        let st = &self.sys_state;
        let global = self.manual_pause
            || (g.pause_on_lock && self.locked)
            || (g.pause_on_display_off && self.display_off)
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

        self.update_audio();
    }
}
