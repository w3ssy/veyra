//! Video: Dekoder-Thread (pro Datei einmal, auch wenn mehrere Monitore sie zeigen) + Wiedergabe-Uhr.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

#[cfg(windows)]
mod mf;

/// Vorschaubild als RGBA (Breite, Höhe, Pixel) – für das Einstellungsfenster.
#[cfg(windows)]
pub use mf::thumbnail;

/// Ein dekodiertes Bild im NV12-Format (Y-Ebene, danach verschachtelte UV-Ebene).
/// NV12 ist das native Ausgabeformat der Hardware-Dekoder und halb so groß wie RGBA.
pub struct Frame {
    pub data: Vec<u8>,
    /// Bytes pro Zeile (gilt für beide Ebenen).
    pub pitch: u32,
    /// Kodierte Größe (kann z. B. 1088 statt 1080 Zeilen sein).
    pub width: u32,
    pub height: u32,
    /// Zeilen der Y-Ebene im Puffer (≥ height, Hardware richtet manchmal aus).
    pub y_rows: u32,
    /// Sichtbarer Ausschnitt in Texturkoordinaten: [offset_x, offset_y, scale_x, scale_y].
    pub crop: [f32; 4],
    /// Darstellungszeitpunkt, über Schleifendurchläufe hinweg monoton steigend.
    pub pts: Duration,
    /// BT.709 (HD) statt BT.601 (SD) Farbmatrix.
    pub bt709: bool,
}

impl Frame {
    pub fn uv_offset(&self) -> usize {
        self.pitch as usize * self.y_rows as usize
    }
}

/// Wie dekodiert werden soll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DecodeOptions {
    /// Größter Monitor, der das Video zeigt (Breite, Höhe). Größere Videos verkleinert die GPU
    /// schon beim Dekodieren darauf – 4K auf einem 1080p-Monitor = ¼ der Daten.
    pub max_size: Option<(u32, u32)>,
    /// Sparsame (integrierte) statt leistungsstarker GPU – dieselbe Wahl wie beim Zeichnen.
    pub low_power: bool,
}

/// Ausgabegröße: so weit verkleinern, dass das Video den Monitor gerade noch füllt (wie `fit = "cover"`),
/// nie vergrößern. Gerade Werte, weil NV12 Farbinformation pro 2×2 Pixel speichert.
pub fn scaled_size(video: (u32, u32), monitor: (u32, u32)) -> Option<(u32, u32)> {
    let (vw, vh) = (video.0 as f64, video.1 as f64);
    let s = (monitor.0 as f64 / vw).max(monitor.1 as f64 / vh);
    if s >= 0.95 {
        return None; // lohnt sich nicht
    }
    let even = |x: f64| ((x.ceil() as u32).div_ceil(2) * 2).max(2);
    Some((even(vw * s), even(vh * s)))
}

/// Wie viele Bilder der Dekoder vorausarbeiten darf. Klein halten = wenig RAM/VRAM.
const QUEUE: usize = 3;

pub struct Player {
    pub path: PathBuf,
    rx: Receiver<Frame>,
    pool: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    pending: Option<Frame>,
    position: Duration,
    started: bool,
    failed: bool,
}

impl Player {
    pub fn open(path: &Path, opts: DecodeOptions) -> Self {
        let (tx, rx) = sync_channel(QUEUE);
        let (pool_tx, pool_rx) = sync_channel(QUEUE + 2);
        let stop = Arc::new(AtomicBool::new(false));
        spawn_decoder(path.to_path_buf(), opts, tx, pool_rx, stop.clone());
        Self { path: path.to_path_buf(), rx, pool: pool_tx, stop, pending: None, position: Duration::ZERO, started: false, failed: false }
    }

    /// Uhr vorstellen. Große Sprünge (Standby, Debugger) werden gekappt, damit nicht "vorgespult" wird.
    pub fn advance(&mut self, dt: Duration) {
        if self.started {
            self.position += dt.min(Duration::from_millis(100));
        }
    }

    /// Wie lange, bis das nächste Bild fällig ist (`None`, wenn der Dekoder noch keins geliefert hat).
    pub fn next_due(&mut self) -> Option<Duration> {
        if self.pending.is_none() {
            self.pending = self.rx.try_recv().ok();
        }
        let f = self.pending.as_ref()?;
        Some(if self.started { f.pts.saturating_sub(self.position) } else { Duration::ZERO })
    }

    /// Das neueste fällige Bild holen (ältere fällige werden übersprungen und recycelt).
    pub fn take_due(&mut self) -> Option<Frame> {
        let mut due: Option<Frame> = None;
        loop {
            if self.pending.is_none() {
                match self.rx.try_recv() {
                    Ok(f) => self.pending = Some(f),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        if !self.failed {
                            log::error!("Dekoder für {:?} beendet (Datei fehlt oder Format nicht unterstützt?)", self.path);
                            self.failed = true;
                        }
                        break;
                    }
                }
            }
            let f = self.pending.as_ref().unwrap();
            if !self.started {
                self.position = f.pts;
                self.started = true;
            }
            if f.pts <= self.position {
                if let Some(old) = due.replace(self.pending.take().unwrap()) {
                    self.recycle(old);
                }
            } else {
                break;
            }
        }
        due
    }

    /// Puffer an den Dekoder zurückgeben – spart eine Allokation pro Bild.
    pub fn recycle(&self, f: Frame) {
        let _ = self.pool.try_send(f.data);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn spawn_decoder(path: PathBuf, opts: DecodeOptions, tx: SyncSender<Frame>, pool: Receiver<Vec<u8>>, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("video-decoder".into())
        .spawn(move || {
            if let Err(e) = mf::decode_loop(&path, opts, &tx, &pool, &stop) {
                log::error!("Video {path:?}: {e:?}");
            }
        })
        .expect("Dekoder-Thread starten");
}

#[cfg(not(windows))]
fn spawn_decoder(path: PathBuf, _opts: DecodeOptions, _tx: SyncSender<Frame>, _pool: Receiver<Vec<u8>>, _stop: Arc<AtomicBool>) {
    // Linux-Backend (GStreamer/VA-API) folgt – siehe docs/ARCHITEKTUR.md.
    log::error!("Kein Video-Backend für diese Plattform: {path:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ms: u64) -> Frame {
        Frame { data: vec![], pitch: 4, width: 4, height: 2, y_rows: 2, crop: [0.0, 0.0, 1.0, 1.0], pts: Duration::from_millis(ms), bt709: true }
    }

    #[test]
    fn next_due_reports_time_to_next_frame() {
        let (tx, rx) = sync_channel(8);
        let (pool, _pool_rx) = sync_channel(8);
        let mut p = Player { path: "x".into(), rx, pool, stop: Default::default(), pending: None, position: Duration::ZERO, started: false, failed: false };
        assert_eq!(p.next_due(), None);
        tx.send(frame(1000)).unwrap();
        tx.send(frame(1016)).unwrap();
        assert_eq!(p.next_due(), Some(Duration::ZERO));
        p.take_due().unwrap();
        assert_eq!(p.next_due(), Some(Duration::from_millis(16)));
        p.advance(Duration::from_millis(10));
        assert_eq!(p.next_due(), Some(Duration::from_millis(6)));
    }

    #[test]
    fn downscale_to_monitor() {
        // 4K auf 1080p: exakt ein Viertel.
        assert_eq!(scaled_size((3840, 2160), (1920, 1080)), Some((1920, 1080)));
        // Anderes Seitenverhältnis: so groß, dass der Monitor gefüllt wird.
        assert_eq!(scaled_size((3840, 1600), (1920, 1080)), Some((2592, 1080)));
        // Kleiner oder gleich groß: nichts tun.
        assert_eq!(scaled_size((1920, 1080), (1920, 1080)), None);
        assert_eq!(scaled_size((1280, 720), (2560, 1440)), None);
    }

    #[test]
    fn clock_picks_latest_due_frame() {
        let (tx, rx) = sync_channel(8);
        let (pool, _pool_rx) = sync_channel(8);
        let mut p = Player { path: "x".into(), rx, pool, stop: Default::default(), pending: None, position: Duration::ZERO, started: false, failed: false };
        for ms in [1000, 1033, 1066, 1100] {
            tx.send(frame(ms)).unwrap();
        }
        // Erstes Bild startet die Uhr bei seinem Zeitstempel.
        assert_eq!(p.take_due().unwrap().pts.as_millis(), 1000);
        assert!(p.take_due().is_none());
        p.advance(Duration::from_millis(70));
        // 1033 wird übersprungen, 1066 ist das neueste fällige Bild.
        assert_eq!(p.take_due().unwrap().pts.as_millis(), 1066);
        // Riesiger Zeitsprung wird auf 100 ms gekappt.
        p.advance(Duration::from_secs(30));
        assert_eq!(p.take_due().unwrap().pts.as_millis(), 1100);
    }
}
