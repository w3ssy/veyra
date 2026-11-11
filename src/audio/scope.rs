//! Messdaten für das Widget *Signal Analyzer*: die letzten Stereo-Samples (Oszilloskop, XY) und ein
//! Spektrum in logarithmisch verteilten Bändern mit Spitzenwert-Anzeige – wie ein Hardware-Analyzer.
//!
//! Läuft im Audio-Thread; das Widget holt sich pro Bild nur eine Kopie von [`Scope`].

use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::{Arc, Mutex};

/// Anzahl der Spektrum-Bänder.
pub const BANDS: usize = 48;
/// So viele Stereo-Samples liegen bereit (bei 48 kHz ≈ 43 ms).
pub const SCOPE_LEN: usize = 2048;
const FFT_SIZE: usize = SCOPE_LEN;
pub const LOW_HZ: f32 = 40.0;
pub const HIGH_HZ: f32 = 16_000.0;
/// Anzeigebereich des Spektrums in dBFS.
pub const DB_FLOOR: f32 = -78.0;
pub const DB_CEIL: f32 = -12.0;
/// Takte (à ~16 ms), die ein Spitzenwert stehen bleibt, bevor er fällt.
const PEAK_HOLD: u32 = 30;

/// Momentaufnahme für das Widget.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    /// Gerade ist etwas zu hören.
    pub active: bool,
    pub rate: u32,
    /// Chronologisch, ältestes Sample zuerst.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Pegel je Band, 0 … 1 (zwischen [`DB_FLOOR`] und [`DB_CEIL`]).
    pub bands: Vec<f32>,
    /// Gehaltene Spitzenwerte je Band, 0 … 1.
    pub peaks: Vec<f32>,
    /// Höchster Sample-Wert der letzten Sekunde in dBFS.
    pub peak_db: f32,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            active: false,
            rate: 0,
            left: vec![0.0; SCOPE_LEN],
            right: vec![0.0; SCOPE_LEN],
            bands: vec![0.0; BANDS],
            peaks: vec![0.0; BANDS],
            peak_db: f32::NEG_INFINITY,
        }
    }
}

pub type SharedScope = Arc<Mutex<Scope>>;

pub struct ScopeTap {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    scratch: Vec<Complex32>,
    /// Band-Grenzen als (Bin-Start, Bin-Ende, Mittenfrequenz in Bins).
    edges: Vec<(usize, usize, f32)>,
    bands: Vec<f32>,
    peaks: Vec<f32>,
    hold: Vec<u32>,
    rate: u32,
    /// Takte seit dem letzten hörbaren Sample.
    quiet: u32,
    /// Spitzenwert seit `sec_start` und der der vorigen vollen Sekunde.
    sec_peak: f32,
    shown_peak: f32,
    ticks: u32,
}

impl ScopeTap {
    pub fn new(rate: u32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()).collect();
        let bin_hz = rate.max(1) as f32 / FFT_SIZE as f32;
        let high = HIGH_HZ.min(rate as f32 * 0.45);
        let ratio = (high / LOW_HZ).powf(1.0 / BANDS as f32);
        let edges = (0..BANDS)
            .map(|i| {
                let lo = LOW_HZ * ratio.powi(i as i32);
                let hi = lo * ratio;
                let a = (lo / bin_hz).ceil() as usize;
                let b = ((hi / bin_hz).ceil() as usize).min(FFT_SIZE / 2);
                (a.max(1), b, (lo * hi).sqrt() / bin_hz)
            })
            .collect();
        Self {
            fft,
            window,
            left: vec![0.0; SCOPE_LEN],
            right: vec![0.0; SCOPE_LEN],
            pos: 0,
            scratch: vec![Complex32::default(); FFT_SIZE],
            edges,
            bands: vec![0.0; BANDS],
            peaks: vec![0.0; BANDS],
            hold: vec![0; BANDS],
            rate,
            quiet: u32::MAX,
            sec_peak: 0.0,
            shown_peak: 0.0,
            ticks: 0,
        }
    }

    /// Ein Stereo-Sample einspeisen.
    #[inline]
    pub fn push(&mut self, l: f32, r: f32) {
        self.left[self.pos] = l;
        self.right[self.pos] = r;
        self.pos = (self.pos + 1) % SCOPE_LEN;
        let a = l.abs().max(r.abs());
        if a > 1e-4 {
            self.quiet = 0;
        }
        self.sec_peak = self.sec_peak.max(a);
    }

    /// Einmal pro Analyse-Takt (~16 ms): Spektrum rechnen und in `out` schreiben.
    pub fn publish(&mut self, out: &mut Scope) {
        self.quiet = self.quiet.saturating_add(1);
        // Kurze Pausen (zwischen zwei Liedern …) lassen das Bild nicht sofort erlöschen.
        let active = self.quiet < 20;

        for i in 0..FFT_SIZE {
            let k = (self.pos + i) % SCOPE_LEN;
            let m = if active { (self.left[k] + self.right[k]) * 0.5 } else { 0.0 };
            self.scratch[i] = Complex32::new(m * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        // Ein Sinus mit Amplitude 1 ergibt mit Hann-Fenster im Maximum N/4 → 0 dBFS.
        let norm = 4.0 / FFT_SIZE as f32;
        let mag = |i: usize| self.scratch[i.min(FFT_SIZE / 2)].norm() * norm;
        for (b, &(lo, hi, center)) in self.edges.iter().enumerate() {
            let m = if hi > lo {
                (lo..hi).map(mag).fold(0.0, f32::max)
            } else {
                // Schmaler als ein Bin (tiefe Bänder): zwischen den Nachbar-Bins interpolieren.
                let i = center.floor() as usize;
                let f = center.fract();
                mag(i) * (1.0 - f) + mag(i + 1) * f
            };
            let db = 20.0 * m.max(1e-9).log10();
            let level = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);
            // Sofort hoch, langsam runter.
            self.bands[b] = if level >= self.bands[b] { level } else { (self.bands[b] * 0.86).max(level) };
            if self.bands[b] >= self.peaks[b] {
                self.peaks[b] = self.bands[b];
                self.hold[b] = PEAK_HOLD;
            } else if self.hold[b] > 0 {
                self.hold[b] -= 1;
            } else {
                self.peaks[b] = (self.peaks[b] - 0.012).max(self.bands[b]);
            }
        }

        self.ticks += 1;
        if self.ticks >= 60 {
            self.ticks = 0;
            self.shown_peak = self.sec_peak;
            self.sec_peak = 0.0;
        }

        out.active = active;
        out.rate = self.rate;
        out.left.clear();
        out.right.clear();
        out.left.extend(self.left[self.pos..].iter().chain(&self.left[..self.pos]));
        out.right.extend(self.right[self.pos..].iter().chain(&self.right[..self.pos]));
        out.bands.clone_from(&self.bands);
        out.peaks.clone_from(&self.peaks);
        let p = self.shown_peak.max(self.sec_peak);
        out.peak_db = if p > 1e-6 { 20.0 * p.log10() } else { f32::NEG_INFINITY };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tap: &mut ScopeTap, hz: f32, amp: f32, n: usize) {
        for i in 0..n {
            let s = amp * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin();
            tap.push(s, s);
        }
    }

    #[test]
    fn tone_lights_its_band() {
        let mut tap = ScopeTap::new(48_000);
        let mut out = Scope::default();
        feed(&mut tap, 1000.0, 0.5, 4096);
        tap.publish(&mut out);
        assert!(out.active);
        let (best, _) = out.bands.iter().enumerate().fold((0, 0.0), |a, (i, &v)| if v > a.1 { (i, v) } else { a });
        // Band mit 1 kHz: LOW_HZ · ratio^i ≤ 1000 < LOW_HZ · ratio^(i+1)
        let ratio = (HIGH_HZ / LOW_HZ).powf(1.0 / BANDS as f32);
        let expected = ((1000.0 / LOW_HZ).ln() / ratio.ln()).floor() as usize;
        assert!(best.abs_diff(expected) <= 1, "{best} vs {expected}");
        assert!(out.bands[best] > 0.8, "−6 dBFS liegt weit oben: {}", out.bands[best]);
        assert!(out.bands[2] < 0.2, "tiefe Bänder bleiben dunkel");
        assert!((out.peak_db - 20.0 * 0.5f32.log10()).abs() < 0.2);
        assert_eq!(out.left.len(), SCOPE_LEN);
    }

    #[test]
    fn silence_goes_dark() {
        let mut tap = ScopeTap::new(48_000);
        let mut out = Scope::default();
        feed(&mut tap, 200.0, 0.8, 2048);
        tap.publish(&mut out);
        for _ in 0..150 {
            tap.publish(&mut out);
        }
        assert!(!out.active);
        assert!(out.bands.iter().all(|&b| b < 0.01));
        assert!(out.peaks.iter().all(|&b| b < 0.01), "Spitzenwerte fallen nach dem Halten");
    }
}
