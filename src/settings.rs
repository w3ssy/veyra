//! Einstellungsfenster (`veyra.exe --settings`): eigener Prozess mit egui.
//!
//! Es liest und schreibt ausschließlich `config.toml` – der Hintergrund-Prozess erkennt die Änderung
//! und übernimmt sie. Ist das Fenster zu, kostet es nichts; stürzt es ab, läuft das Wallpaper weiter.

use crate::config::{self, Backend, Config, Effect, Fit, GpuPreference, MonitorSel, Wallpaper};
use crate::platform::sys::{self, Monitor};
use anyhow::{anyhow, Result};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Instant, SystemTime};

const ACCENT: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
const CYAN: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE);
const THUMB_W: u32 = 320;

pub fn run() -> Result<()> {
    let Some(_guard) = sys::settings_instance() else {
        sys::focus_settings();
        return Ok(());
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(sys::SETTINGS_TITLE)
        .with_inner_size([1000.0, 720.0])
        .with_min_inner_size([780.0, 560.0])
        .with_drag_and_drop(true);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Veyra", options, Box::new(|cc| Ok(Box::new(Settings::new(&cc.egui_ctx))))).map_err(|e| anyhow!("{e}"))
}

/// Fenstersymbol aus dem eingebetteten Programm-Icon (64-px-Bitmap aus `assets/veyra.ico`).
fn window_icon() -> Option<egui::IconData> {
    let ico: &[u8] = include_bytes!("../assets/veyra.ico");
    let u16_at = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]) as usize;
    (0..u16_at(4)).find_map(|i| {
        let e = 6 + i * 16;
        let size = ico[e] as usize;
        if size != 64 {
            return None;
        }
        let data = &ico[u32_at(e + 12)..];
        let pixels = data.get(40..40 + size * size * 4)?; // BITMAPINFOHEADER, dann BGRA von unten nach oben
        let mut rgba = Vec::with_capacity(size * size * 4);
        for row in pixels.chunks_exact(size * 4).rev() {
            for p in row.chunks_exact(4) {
                rgba.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        Some(egui::IconData { rgba, width: size as u32, height: size as u32 })
    })
}

// ───────────────────────────── Vorschaubilder ─────────────────────────────

/// Fertig dekodiertes Vorschaubild: Breite, Höhe, RGBA.
type ThumbResult = (PathBuf, Result<(u32, u32, Vec<u8>)>);

enum Thumb {
    Loading,
    Ready(TextureHandle),
    Failed,
}

/// Dekodiert Vorschaubilder nacheinander in einem Hintergrund-Thread.
struct Thumbs {
    map: HashMap<PathBuf, Thumb>,
    request: Sender<PathBuf>,
    done: Receiver<ThumbResult>,
}

impl Thumbs {
    fn new(ctx: &egui::Context) -> Self {
        let (request, jobs) = channel::<PathBuf>();
        let (result, done) = channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("thumbnails".into())
            .spawn(move || {
                while let Ok(path) = jobs.recv() {
                    let r = crate::video::thumbnail(&path, THUMB_W);
                    if let Err(e) = &r {
                        log::warn!("Vorschaubild {path:?}: {e:#}");
                    }
                    if result.send((path, r)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("Vorschau-Thread starten");
        Self { map: HashMap::new(), request, done }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, r)) = self.done.try_recv() {
            let t = match r {
                Ok((w, h, rgba)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    Thumb::Ready(ctx.load_texture(path.to_string_lossy(), img, egui::TextureOptions::LINEAR))
                }
                Err(_) => Thumb::Failed,
            };
            self.map.insert(path, t);
        }
    }

    fn get(&mut self, path: &Path) -> &Thumb {
        if !self.map.contains_key(path) {
            let _ = self.request.send(path.to_path_buf());
            self.map.insert(path.to_path_buf(), Thumb::Loading);
        }
        &self.map[path]
    }
}

/// Bild so in `rect` zeichnen, dass es die Fläche füllt (wie `fit = "cover"`).
fn paint_cover(painter: &egui::Painter, rect: Rect, tex: &TextureHandle) {
    let [w, h] = tex.size();
    let (ia, ra) = (w as f32 / h as f32, rect.width() / rect.height());
    let uv = if ia > ra {
        let u = ra / ia;
        Rect::from_min_max(Pos2::new(0.5 - u / 2.0, 0.0), Pos2::new(0.5 + u / 2.0, 1.0))
    } else {
        let v = ia / ra;
        Rect::from_min_max(Pos2::new(0.0, 0.5 - v / 2.0), Pos2::new(1.0, 0.5 + v / 2.0))
    };
    painter.image(tex.id(), rect, uv, Color32::WHITE);
}

// ───────────────────────────── Zustand ─────────────────────────────

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Wallpaper,
    General,
}

struct Settings {
    path: PathBuf,
    mtime: Option<SystemTime>,
    /// Allgemeine Einstellungen, Audio, Bibliothek (Wallpaper-Liste wird aus `all`/`per` gebaut).
    cfg: Config,
    /// Zustand der Datei nach dem letzten Laden/Speichern (für „ungespeicherte Änderungen“).
    baseline: Config,
    monitors: Vec<Monitor>,
    same_for_all: bool,
    all: Wallpaper,
    per: Vec<Wallpaper>,
    selected: usize,
    /// Monitor-Rechtecke der Übersicht aus dem letzten Bild (für Drag & Drop).
    monitor_rects: Vec<Rect>,
    library: Vec<PathBuf>,
    thumbs: Thumbs,
    tab: Tab,
    status: Option<(String, bool, Instant)>,
}

impl Settings {
    fn new(ctx: &egui::Context) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.selection.bg_fill = ACCENT;
        visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        visuals.hyperlink_color = CYAN;
        visuals.panel_fill = Color32::from_rgb(0x14, 0x13, 0x1F);
        visuals.window_fill = Color32::from_rgb(0x1B, 0x1A, 0x29);
        visuals.extreme_bg_color = Color32::from_rgb(0x0E, 0x0D, 0x16);
        ctx.set_visuals(visuals);

        let path = config::config_path();
        let mut s = Self {
            mtime: None,
            cfg: Config::default(),
            baseline: Config::default(),
            monitors: sys::monitors(),
            same_for_all: true,
            all: Wallpaper::default(),
            per: Vec::new(),
            selected: 0,
            monitor_rects: Vec::new(),
            library: Vec::new(),
            thumbs: Thumbs::new(ctx),
            tab: Tab::Wallpaper,
            status: None,
            path,
        };
        s.selected = s.monitors.iter().position(|m| m.primary).unwrap_or(0);
        s.load();
        s
    }

    fn load(&mut self) {
        let cfg = match Config::load_or_create(&self.path) {
            Ok(c) => c,
            Err(e) => {
                self.set_status(format!("config.toml fehlerhaft – es werden Standardwerte angezeigt ({e:#})"), true);
                Config::default()
            }
        };
        self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        let primary: Vec<bool> = self.monitors.iter().map(|m| m.primary).collect();
        self.per = cfg
            .assign(&primary)
            .into_iter()
            .enumerate()
            .map(|(i, w)| Wallpaper { monitor: MonitorSel::Index(i + 1), ..w.cloned().unwrap_or_default() })
            .collect();
        let same = |a: &Wallpaper, b: &Wallpaper| a.video == b.video && a.fit == b.fit && a.effect == b.effect && a.intensity == b.intensity;
        self.same_for_all = self.per.windows(2).all(|p| same(&p[0], &p[1]));
        self.all = Wallpaper { monitor: MonitorSel::All, ..self.per.first().cloned().unwrap_or_default() };
        self.cfg = cfg;
        self.library = self.cfg.library.videos();
        self.baseline = self.compose();
    }

    /// Konfiguration aus dem Fensterzustand zusammensetzen.
    fn compose(&self) -> Config {
        let mut cfg = self.cfg.clone();
        cfg.wallpapers = if self.same_for_all || self.per.is_empty() {
            vec![self.all.clone()]
        } else {
            let list: Vec<Wallpaper> = self.per.iter().filter(|w| !w.video.as_os_str().is_empty()).cloned().collect();
            if list.is_empty() {
                vec![Wallpaper::default()]
            } else {
                list
            }
        };
        cfg.sanitize();
        cfg
    }

    fn dirty(&self) -> bool {
        self.compose() != self.baseline
    }

    fn save(&mut self) {
        let cfg = self.compose();
        match config::save(&self.path, &cfg) {
            Ok(()) => {
                self.baseline = cfg;
                self.mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
                let msg = if sys::wallpaper_running() { "Gespeichert – Veyra übernimmt die Änderungen sofort." } else { "Gespeichert. Veyra läuft gerade nicht." };
                self.set_status(msg.into(), false);
            }
            Err(e) => self.set_status(format!("Speichern fehlgeschlagen: {e:#}"), true),
        }
    }

    fn set_status(&mut self, text: String, error: bool) {
        self.status = Some((text, error, Instant::now()));
    }

    /// Der Wallpaper-Eintrag, den die Bedienelemente gerade bearbeiten.
    fn current(&mut self) -> &mut Wallpaper {
        if self.same_for_all || self.per.is_empty() {
            &mut self.all
        } else {
            let i = self.selected.min(self.per.len() - 1);
            &mut self.per[i]
        }
    }

    fn video_for(&self, monitor: usize) -> &Path {
        if self.same_for_all {
            &self.all.video
        } else {
            self.per.get(monitor).map(|w| w.video.as_path()).unwrap_or(Path::new(""))
        }
    }

    fn assign(&mut self, monitor: Option<usize>, video: PathBuf) {
        if let Some(m) = monitor {
            self.selected = m;
        }
        self.current().video = video;
    }

    /// Datei wurde außerhalb geändert (Tray-Schalter, Editor) → neu laden, solange hier nichts offen ist.
    fn watch_file(&mut self) {
        let m = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if m != self.mtime && !self.dirty() {
            self.load();
        }
    }
}

fn fit_label(f: Fit) -> &'static str {
    match f {
        Fit::Cover => "Füllen (Ränder werden beschnitten)",
        Fit::Contain => "Einpassen (schwarze Ränder)",
        Fit::Stretch => "Strecken (verzerrt)",
    }
}

fn effect_label(e: Effect) -> &'static str {
    match e {
        Effect::None => "Kein Effekt",
        Effect::Pulse => "Pulsieren (Bass)",
        Effect::Glow => "Leuchten",
        Effect::Chroma => "Farbverschiebung",
    }
}

/// Kleiner farbiger Punkt (das Zeichen „●“ fehlt in der eingebauten Schrift).
fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ───────────────────────────── Oberfläche ─────────────────────────────

impl eframe::App for Settings {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.thumbs.poll(&ctx);
        self.watch_file();
        ctx.request_repaint_after(std::time::Duration::from_secs(1));

        // Strg+S speichert.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) && self.dirty() {
            self.save();
        }

        self.handle_drop(&ctx);

        egui::Panel::top("header").frame(egui::Frame::new().fill(Color32::from_rgb(0x0E, 0x0D, 0x16)).inner_margin(egui::Margin::symmetric(18, 12))).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Veyra").size(26.0).strong().color(Color32::WHITE));
                ui.label(RichText::new("Motion for your universe.").size(14.0).color(CYAN));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.tab, Tab::General, RichText::new("Allgemein").size(15.0));
                    ui.selectable_value(&mut self.tab, Tab::Wallpaper, RichText::new("Wallpaper").size(15.0));
                });
            });
        });

        egui::Panel::bottom("footer").frame(egui::Frame::new().fill(Color32::from_rgb(0x0E, 0x0D, 0x16)).inner_margin(egui::Margin::symmetric(18, 10))).show(ui, |ui| {
            ui.horizontal(|ui| {
                if !sys::wallpaper_running() {
                    status_dot(ui, Color32::from_rgb(0xF5, 0x9E, 0x0B));
                    ui.label(RichText::new("Veyra läuft nicht").color(Color32::from_rgb(0xF5, 0x9E, 0x0B)));
                    if ui.button("Starten").clicked() {
                        sys::spawn_self(&[]);
                    }
                } else {
                    status_dot(ui, Color32::from_rgb(0x34, 0xD3, 0x99));
                    ui.label(RichText::new("Veyra läuft").color(Color32::from_rgb(0x34, 0xD3, 0x99)));
                }
                if let Some((text, error, at)) = &self.status {
                    if at.elapsed().as_secs() < 8 {
                        ui.separator();
                        ui.label(RichText::new(text).color(if *error { Color32::from_rgb(0xF8, 0x71, 0x71) } else { Color32::LIGHT_GRAY }));
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let dirty = self.dirty();
                    let save = egui::Button::new(RichText::new("Speichern").strong().color(Color32::WHITE)).fill(if dirty { ACCENT } else { Color32::from_gray(60) });
                    if ui.add_enabled(dirty, save).on_hover_text("Strg+S").clicked() {
                        self.save();
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Verwerfen")).clicked() {
                        self.load();
                        self.set_status("Änderungen verworfen.".into(), false);
                    }
                    if dirty {
                        ui.label(RichText::new("Ungespeicherte Änderungen").italics().color(Color32::GRAY));
                    }
                });
            });
        });

        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::from_rgb(0x14, 0x13, 0x1F)).inner_margin(egui::Margin::same(18))).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                Tab::Wallpaper => self.wallpaper_tab(ui),
                Tab::General => self.general_tab(ui),
            });
        });

        // Hinweis beim Darüberziehen von Dateien.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
            painter.text(screen.center(), Align2::CENTER_CENTER, "Video auf einen Monitor ziehen und loslassen", FontId::proportional(22.0), Color32::WHITE);
        }
    }
}

impl Settings {
    fn handle_drop(&mut self, ctx: &egui::Context) {
        let (files, pos) = ctx.input(|i| (i.raw.dropped_files.clone(), i.pointer.hover_pos()));
        let Some(video) = files.into_iter().map(|f| f.path().to_path_buf()).find(|p| config::is_video(p)) else { return };
        // Über welchem Monitor in der Übersicht wurde losgelassen?
        let target = pos.and_then(|p| self.monitor_rects.iter().position(|r| r.contains(p)));
        self.assign(target, video);
        self.set_status("Video zugewiesen – speichern nicht vergessen.".into(), false);
    }

    fn section(ui: &mut egui::Ui, title: &str) {
        ui.add_space(14.0);
        ui.label(RichText::new(title).size(17.0).strong().color(Color32::WHITE));
        ui.add_space(4.0);
    }

    fn wallpaper_tab(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "Monitore");
        if self.monitors.is_empty() {
            ui.label("Keine Monitore gefunden.");
            return;
        }
        self.monitor_layout(ui);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let before = self.same_for_all;
            ui.checkbox(&mut self.same_for_all, "Ein Video für alle Monitore");
            if before && !self.same_for_all {
                // Aufteilen: jeder Monitor startet mit dem bisher gemeinsamen Video.
                for (i, w) in self.per.iter_mut().enumerate() {
                    *w = Wallpaper { monitor: MonitorSel::Index(i + 1), ..self.all.clone() };
                }
            } else if !before && self.same_for_all {
                let i = self.selected.min(self.per.len().saturating_sub(1));
                self.all = Wallpaper { monitor: MonitorSel::All, ..self.per.get(i).cloned().unwrap_or_default() };
            }
            ui.label(RichText::new("· Monitor anklicken zum Auswählen, Videos auch per Drag & Drop").color(Color32::GRAY));
        });

        let title = if self.same_for_all {
            "Alle Monitore".to_string()
        } else {
            let m = &self.monitors[self.selected.min(self.monitors.len() - 1)];
            format!("Monitor {} – {}×{}{}", self.selected + 1, m.width, m.height, if m.primary { " (Hauptmonitor)" } else { "" })
        };
        Self::section(ui, &title);
        let library_dir = self.cfg.library.effective_folder();
        let w = self.current();
        egui::Grid::new("wp").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Video");
            ui.horizontal(|ui| {
                let name = if w.video.as_os_str().is_empty() { "– kein Video (Windows-Hintergrund) –".to_string() } else { file_name(&w.video) };
                ui.label(RichText::new(name).strong()).on_hover_text(w.video.to_string_lossy());
                if ui.button("Durchsuchen…").clicked() {
                    let start = w.video.parent().map(Path::to_path_buf).unwrap_or(library_dir.clone());
                    if let Some(p) = sys::pick_path(false, Some(&start)) {
                        w.video = p;
                    }
                }
                if !w.video.as_os_str().is_empty() && ui.button("Entfernen").clicked() {
                    w.video = PathBuf::new();
                }
            });
            ui.end_row();

            ui.label("Darstellung");
            egui::ComboBox::from_id_salt("fit").width(260.0).selected_text(fit_label(w.fit)).show_ui(ui, |ui| {
                for f in Fit::ALL {
                    ui.selectable_value(&mut w.fit, f, fit_label(f));
                }
            });
            ui.end_row();

            ui.label("Audio-Effekt");
            egui::ComboBox::from_id_salt("effect").width(260.0).selected_text(effect_label(w.effect)).show_ui(ui, |ui| {
                for e in Effect::ALL {
                    ui.selectable_value(&mut w.effect, e, effect_label(e));
                }
            });
            ui.end_row();

            ui.label("Stärke");
            ui.add_enabled(w.effect != Effect::None, egui::Slider::new(&mut w.intensity, 0.0..=3.0).step_by(0.05));
            ui.end_row();
        });

        self.library_view(ui);
    }

    /// Monitore in ihrer echten Anordnung, mit Vorschaubild des zugewiesenen Videos.
    fn monitor_layout(&mut self, ui: &mut egui::Ui) {
        let min_x = self.monitors.iter().map(|m| m.x).min().unwrap_or(0) as f32;
        let min_y = self.monitors.iter().map(|m| m.y).min().unwrap_or(0) as f32;
        let max_x = self.monitors.iter().map(|m| m.x + m.width).max().unwrap_or(1) as f32;
        let max_y = self.monitors.iter().map(|m| m.y + m.height).max().unwrap_or(1) as f32;
        let avail = ui.available_width();
        let scale = (avail / (max_x - min_x)).min(220.0 / (max_y - min_y));
        let size = Vec2::new((max_x - min_x) * scale, (max_y - min_y) * scale);
        let (resp, painter) = ui.allocate_painter(Vec2::new(avail, size.y + 8.0), Sense::click());
        let origin = resp.rect.min + Vec2::new((avail - size.x) / 2.0, 4.0);

        let rects: Vec<Rect> = self
            .monitors
            .iter()
            .map(|m| {
                let min = origin + Vec2::new((m.x as f32 - min_x) * scale, (m.y as f32 - min_y) * scale);
                Rect::from_min_size(min, Vec2::new(m.width as f32 * scale, m.height as f32 * scale)).shrink(4.0)
            })
            .collect();
        self.monitor_rects = rects.clone();

        let hover = resp.hover_pos();
        for (i, rect) in rects.iter().enumerate() {
            let video = self.video_for(i).to_path_buf();
            painter.rect_filled(*rect, CornerRadius::same(6), Color32::from_rgb(0x22, 0x20, 0x33));
            if video.as_os_str().is_empty() {
                painter.text(rect.center(), Align2::CENTER_CENTER, "Windows-Hintergrund", FontId::proportional(13.0), Color32::GRAY);
            } else {
                match self.thumbs.get(&video) {
                    Thumb::Ready(tex) => paint_cover(&painter, *rect, tex),
                    Thumb::Loading => {
                        painter.text(rect.center(), Align2::CENTER_CENTER, "Vorschau lädt…", FontId::proportional(13.0), Color32::GRAY);
                    }
                    Thumb::Failed => {
                        painter.text(rect.center(), Align2::CENTER_CENTER, file_name(&video), FontId::proportional(13.0), Color32::LIGHT_GRAY);
                    }
                }
            }
            let selected = self.same_for_all || i == self.selected;
            let hovered = hover.is_some_and(|p| rect.contains(p));
            let stroke = if selected {
                Stroke::new(3.0, ACCENT)
            } else if hovered {
                Stroke::new(2.0, Color32::from_gray(140))
            } else {
                Stroke::new(1.0, Color32::from_gray(70))
            };
            painter.rect_stroke(*rect, CornerRadius::same(6), stroke, StrokeKind::Outside);
            // Nummer wie in config.toml (von links nach rechts).
            let badge = Rect::from_min_size(rect.min + Vec2::splat(8.0), Vec2::new(if self.monitors[i].primary { 92.0 } else { 26.0 }, 24.0));
            painter.rect_filled(badge, CornerRadius::same(5), Color32::from_black_alpha(190));
            let label = if self.monitors[i].primary { format!("{}  Haupt", i + 1) } else { (i + 1).to_string() };
            painter.text(badge.center(), Align2::CENTER_CENTER, label, FontId::proportional(14.0), Color32::WHITE);
        }
        if resp.clicked() {
            if let Some(i) = resp.interact_pointer_pos().and_then(|p| rects.iter().position(|r| r.contains(p))) {
                self.selected = i;
            }
        }
        if hover.is_some_and(|p| rects.iter().any(|r| r.contains(p))) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }

    fn library_view(&mut self, ui: &mut egui::Ui) {
        let folder = self.cfg.library.effective_folder();
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Bibliothek").size(17.0).strong().color(Color32::WHITE));
            ui.label(RichText::new(folder.to_string_lossy()).color(Color32::GRAY));
            if ui.button("Ordner wählen…").clicked() {
                if let Some(p) = sys::pick_path(true, Some(&folder)) {
                    self.cfg.library.folder = p;
                    self.library = self.cfg.library.videos();
                }
            }
            if ui.button("Aktualisieren").clicked() {
                self.library = self.cfg.library.videos();
            }
            if ui.button("Im Explorer öffnen").clicked() {
                sys::open_path(&folder);
            }
        });
        ui.add_space(6.0);
        if self.library.is_empty() {
            ui.label(RichText::new("Keine Videos in diesem Ordner. Lege Videos hinein oder wähle einen anderen Ordner.").color(Color32::GRAY));
            return;
        }

        let tile = Vec2::new(192.0, 108.0);
        let current = self.current().video.clone();
        let mut chosen = None;
        let videos = self.library.clone();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(12.0, 12.0);
            for v in &videos {
                let (rect, resp) = ui.allocate_exact_size(tile + Vec2::new(0.0, 22.0), Sense::click());
                let img = Rect::from_min_size(rect.min, tile);
                let painter = ui.painter_at(rect);
                painter.rect_filled(img, CornerRadius::same(6), Color32::from_rgb(0x22, 0x20, 0x33));
                match self.thumbs.get(v) {
                    Thumb::Ready(tex) => paint_cover(&painter, img, tex),
                    Thumb::Loading => {
                        painter.text(img.center(), Align2::CENTER_CENTER, "…", FontId::proportional(18.0), Color32::GRAY);
                    }
                    Thumb::Failed => {
                        painter.text(img.center(), Align2::CENTER_CENTER, "keine Vorschau", FontId::proportional(12.0), Color32::GRAY);
                    }
                }
                let active = *v == current;
                let stroke = if active {
                    Stroke::new(3.0, ACCENT)
                } else if resp.hovered() {
                    Stroke::new(2.0, CYAN)
                } else {
                    Stroke::new(1.0, Color32::from_gray(60))
                };
                painter.rect_stroke(img, CornerRadius::same(6), stroke, StrokeKind::Inside);
                let mut name = file_name(v);
                if name.chars().count() > 28 {
                    name = name.chars().take(26).collect::<String>() + "…";
                }
                painter.text(Pos2::new(img.center().x, img.max.y + 12.0), Align2::CENTER_CENTER, name, FontId::proportional(12.5), Color32::LIGHT_GRAY);
                if resp.on_hover_text(v.to_string_lossy()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    chosen = Some(v.clone());
                }
            }
        });
        if let Some(v) = chosen {
            self.current().video = v;
        }
    }

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        let g = &mut self.cfg.general;
        Self::section(ui, "Start");
        ui.checkbox(&mut g.autostart, "Mit Windows starten");

        Self::section(ui, "Automatisch pausieren, wenn …");
        ui.checkbox(&mut g.pause_on_fullscreen, "… ein Spiel oder Vollbild-Video auf dem Monitor läuft");
        ui.checkbox(&mut g.pause_on_maximized, "… ein maximiertes Fenster den Monitor verdeckt");
        ui.checkbox(&mut g.pause_on_battery, "… der Laptop im Akkubetrieb oder Energiesparmodus ist");
        ui.checkbox(&mut g.pause_on_lock, "… der Bildschirm gesperrt ist");
        ui.checkbox(&mut g.pause_on_display_off, "… der Bildschirm ausgeschaltet ist");

        Self::section(ui, "Leistung");
        egui::Grid::new("perf").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Max. Bilder pro Sekunde");
            ui.add(egui::Slider::new(&mut g.max_fps, 15..=144)).on_hover_text("30 spart nochmals deutlich Strom");
            ui.end_row();
            ui.label("Grafikkarte");
            egui::ComboBox::from_id_salt("gpu")
                .width(260.0)
                .selected_text(if g.gpu == GpuPreference::Low { "Sparsam (integrierte GPU)" } else { "Leistung (dedizierte GPU)" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut g.gpu, GpuPreference::Low, "Sparsam (integrierte GPU)");
                    ui.selectable_value(&mut g.gpu, GpuPreference::High, "Leistung (dedizierte GPU)");
                });
            ui.end_row();
            ui.label("Grafik-Schnittstelle");
            egui::ComboBox::from_id_salt("backend").width(260.0).selected_text(g.backend.as_str().to_uppercase()).show_ui(ui, |ui| {
                ui.selectable_value(&mut g.backend, Backend::Auto, "AUTO (empfohlen)");
                ui.selectable_value(&mut g.backend, Backend::Dx12, "DX12");
                ui.selectable_value(&mut g.backend, Backend::Vulkan, "VULKAN");
            });
            ui.end_row();
        });
        ui.label(RichText::new("Grafikkarte und Schnittstelle wirken erst nach einem Neustart von Veyra (Tray → Beenden, dann neu starten).").small().color(Color32::GRAY));

        let a = &mut self.cfg.audio;
        Self::section(ui, "Audio-Reaktion");
        ui.checkbox(&mut a.enabled, "Effekte reagieren auf den Systemsound (nur lokal, nichts wird gespeichert)");
        egui::Grid::new("audio").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Empfindlichkeit");
            ui.add_enabled(a.enabled, egui::Slider::new(&mut a.sensitivity, 0.1..=5.0).step_by(0.05));
            ui.end_row();
            ui.label("Glättung");
            ui.add_enabled(a.enabled, egui::Slider::new(&mut a.smoothing, 0.0..=0.95).step_by(0.01));
            ui.end_row();
        });

        Self::section(ui, "Dateien");
        ui.horizontal(|ui| {
            if ui.button("config.toml öffnen").clicked() {
                sys::open_path(&self.path);
            }
            if ui.button("Ordner mit Log öffnen").clicked() {
                sys::open_path(&config::config_dir());
            }
            ui.label(RichText::new(self.path.to_string_lossy()).color(Color32::GRAY));
        });
    }
}
