//! WASAPI-Loopback: liest das, was Windows gerade ausgibt – ohne Treiber, ohne Fremdbibliothek.

use super::scope::ScopeTap;
use super::{Analyzer, SharedLevels, SharedScope};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::*;

pub struct AudioCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioCapture {
    /// `levels` für die Video-Effekte, `scope` für das Widget *Signal Analyzer*.
    pub fn start(levels: SharedLevels, scope: SharedScope, sensitivity: f32, smoothing: f32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("audio-loopback".into())
            .spawn(move || {
                // Bei Gerätewechsel (Kopfhörer eingesteckt …) einfach neu verbinden.
                while !stop2.load(Ordering::Relaxed) {
                    if let Err(e) = run(&levels, &scope, &stop2, sensitivity, smoothing) {
                        log::warn!("Audio-Loopback unterbrochen: {e}");
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                if let Ok(mut l) = levels.lock() {
                    *l = Default::default();
                }
                if let Ok(mut s) = scope.lock() {
                    *s = Default::default();
                }
            })
            .expect("Audio-Thread starten");
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(levels: &SharedLevels, scope: &SharedScope, stop: &AtomicBool, sensitivity: f32, smoothing: f32) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        let fmt_ptr = client.GetMixFormat()?;
        let fmt = *fmt_ptr;
        let channels = fmt.nChannels.max(1) as usize;
        let rate = fmt.nSamplesPerSec;
        let bits = fmt.wBitsPerSample;
        let is_float = fmt.wFormatTag as u32 == WAVE_FORMAT_IEEE_FLOAT
            || (fmt.wFormatTag as u32 == WAVE_FORMAT_EXTENSIBLE
                && std::ptr::addr_of!((*(fmt_ptr as *const WAVEFORMATEXTENSIBLE)).SubFormat).read_unaligned()
                    == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);

        // 200 ms Puffer (Einheit: 100 ns).
        let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt_ptr, None);
        CoTaskMemFree(Some(fmt_ptr as *const _));
        init?;

        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        log::info!("Audio-Loopback aktiv: {rate} Hz, {channels} Kanäle, {bits} Bit, float={is_float}");

        let mut analyzer = Analyzer::new(rate, sensitivity, smoothing);
        let mut tap = ScopeTap::new(rate);
        let mut mono = Vec::with_capacity(4096);
        let mut last_analysis = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(15));
            let mut got = false;
            while capture.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let n = frames as usize;
                mono.clear();
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    mono.resize(n, 0.0);
                    (0..n).for_each(|_| tap.push(0.0, 0.0));
                } else if is_float && bits == 32 {
                    let s = std::slice::from_raw_parts(data as *const f32, n * channels);
                    for c in s.chunks_exact(channels) {
                        mono.push(c.iter().sum::<f32>() / channels as f32);
                        tap.push(c[0], c[channels.min(2) - 1]);
                    }
                } else if bits == 16 {
                    let s = std::slice::from_raw_parts(data as *const i16, n * channels);
                    for c in s.chunks_exact(channels) {
                        let v = |i: usize| c[i] as f32 / 32768.0;
                        mono.push((0..channels).map(v).sum::<f32>() / channels as f32);
                        tap.push(v(0), v(channels.min(2) - 1));
                    }
                } else {
                    mono.resize(n, 0.0); // exotisches Format → ignorieren
                }
                capture.ReleaseBuffer(frames)?;
                analyzer.push(mono.iter().copied());
                got = true;
            }

            if last_analysis.elapsed() >= Duration::from_millis(16) {
                last_analysis = Instant::now();
                let l = if got {
                    analyzer.analyze()
                } else {
                    analyzer.decay();
                    analyzer.analyze_silent()
                };
                if let Ok(mut shared) = levels.lock() {
                    *shared = l;
                }
                if let Ok(mut shared) = scope.lock() {
                    tap.publish(&mut shared);
                }
            }
        }
        let _ = client.Stop();
        Ok(())
    }
}

impl Analyzer {
    /// Aktuelle (abklingende) Werte ohne neue FFT.
    fn analyze_silent(&self) -> super::Levels {
        self.smoothed
    }
}
