//! Audio-Reaktion: Systemsound mitschneiden (Loopback) und in wenige Pegel zerlegen.
//!
//! Plattformunabhängig ist nur die Analyse. Das Mitschneiden ist pro OS implementiert
//! (Windows: WASAPI-Loopback; Linux später: PipeWire/PulseAudio-Monitor).

use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::{Arc, Mutex};

pub mod scope;
#[cfg(windows)]
mod wasapi;
#[cfg(windows)]
pub use wasapi::AudioCapture;
pub use scope::{Scope, SharedScope};

/// Pegel 0.0 … ~1.0, die an den Shader gehen.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Levels {
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
    pub level: f32,
}

pub type SharedLevels = Arc<Mutex<Levels>>;

const FFT_SIZE: usize = 1024;

pub struct Analyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    pos: usize,
    scratch: Vec<Complex32>,
    sample_rate: f32,
    peaks: [f32; 4],
    smoothed: Levels,
    pub sensitivity: f32,
    pub smoothing: f32,
}

impl Analyzer {
    pub fn new(sample_rate: u32, sensitivity: f32, smoothing: f32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            .collect();
        Self {
            fft,
            window,
            ring: vec![0.0; FFT_SIZE],
            pos: 0,
            scratch: vec![Complex32::default(); FFT_SIZE],
            sample_rate: sample_rate as f32,
            peaks: [0.05; 4],
            smoothed: Levels::default(),
            sensitivity,
            smoothing,
        }
    }

    /// Mono-Samples einspeisen (Ringpuffer, es zählen nur die letzten 1024).
    pub fn push(&mut self, samples: impl IntoIterator<Item = f32>) {
        for s in samples {
            self.ring[self.pos] = s;
            self.pos = (self.pos + 1) % FFT_SIZE;
        }
    }

    /// Stille einspeisen (z. B. wenn WASAPI keine Pakete liefert, weil nichts spielt).
    pub fn decay(&mut self) {
        self.smoothed.bass *= 0.85;
        self.smoothed.mid *= 0.85;
        self.smoothed.high *= 0.85;
        self.smoothed.level *= 0.85;
    }

    pub fn analyze(&mut self) -> Levels {
        for i in 0..FFT_SIZE {
            let s = self.ring[(self.pos + i) % FFT_SIZE];
            self.scratch[i] = Complex32::new(s * self.window[i], 0.0);
        }
        self.fft.process(&mut self.scratch);

        let bin_hz = self.sample_rate / FFT_SIZE as f32;
        let band = |lo: f32, hi: f32, spec: &[Complex32]| -> f32 {
            let a = ((lo / bin_hz) as usize).max(1);
            let b = ((hi / bin_hz) as usize).clamp(a + 1, FFT_SIZE / 2);
            let sum: f32 = spec[a..b].iter().map(|c| c.norm()).sum();
            sum / (b - a) as f32
        };
        let raw = [
            band(20.0, 250.0, &self.scratch),
            band(250.0, 2000.0, &self.scratch),
            band(2000.0, 8000.0, &self.scratch),
            self.ring.iter().map(|s| s * s).sum::<f32>().sqrt() / (FFT_SIZE as f32).sqrt(),
        ];

        // Automatische Verstärkung: jeder Kanal relativ zu seinem langsam fallenden Spitzenwert.
        let mut norm = [0.0f32; 4];
        for i in 0..4 {
            self.peaks[i] = (self.peaks[i] * 0.995).max(raw[i]).max(1e-4);
            norm[i] = ((raw[i] / self.peaks[i]) * self.sensitivity).clamp(0.0, 1.5);
        }

        // Schneller Anstieg, langsamer Abfall.
        let k = self.smoothing;
        let mix = |old: f32, new: f32| if new > old { old * (k * 0.5) + new * (1.0 - k * 0.5) } else { old * k + new * (1.0 - k) };
        self.smoothed = Levels {
            bass: mix(self.smoothed.bass, norm[0]),
            mid: mix(self.smoothed.mid, norm[1]),
            high: mix(self.smoothed.high, norm[2]),
            level: mix(self.smoothed.level, norm[3]),
        };
        self.smoothed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bass_tone_hits_bass_band() {
        let mut a = Analyzer::new(48_000, 1.0, 0.0);
        let tone = (0..4096).map(|i| (2.0 * std::f32::consts::PI * 80.0 * i as f32 / 48_000.0).sin());
        a.push(tone);
        let l = a.analyze();
        // Der Bass-Spitzenwert wird vom Ton selbst gesetzt → ~1.0, Höhen bleiben klein.
        assert!(l.bass > 0.9, "{l:?}");
        assert!(l.high < l.bass, "{l:?}");
    }

    #[test]
    fn silence_decays() {
        let mut a = Analyzer::new(48_000, 1.0, 0.5);
        a.push((0..2048).map(|i| (i as f32 * 0.05).sin()));
        let loud = a.analyze();
        for _ in 0..30 {
            a.decay();
        }
        assert!(a.smoothed.level < loud.level * 0.05);
    }
}
