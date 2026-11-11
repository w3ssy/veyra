//! Standbilder als Wallpaper: Ein Bild ist hier einfach ein „Video“ mit genau einem Frame.
//!
//! Der Dekoder-Thread liest die Datei einmal (Windows Imaging Component, nur eingebaute Decoder), verkleinert
//! sie dabei wie ein Video auf die Monitorgröße, wandelt sie in NV12 (BT.709) um und liefert dieses eine Bild.
//! Danach ruht er, bis der Player verworfen wird. Renderer, Darstellung (cover/contain/stretch), Effekte und
//! Widgets funktionieren dadurch unverändert – und ohne neue Bilder wird nur noch gezeichnet, wenn Widgets oder
//! Effekte es verlangen.

use super::{scaled_size, DecodeOptions, Frame, Recycled};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::time::Duration;

/// Vormultipliziertes RGBA (über Schwarz) → NV12 im begrenzten Wertebereich mit BT.709-Matrix, passend zum
/// Shader. `w` und `h` müssen gerade sein. Zeilenlänge (pitch) = `w`.
pub fn rgba_to_nv12(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    debug_assert!(w.is_multiple_of(2) && h.is_multiple_of(2) && rgba.len() >= w * h * 4);
    let mut out = vec![0u8; w * h + w * h / 2];
    let (kr, kb) = (0.2126f32, 0.0722f32);
    let kg = 1.0 - kr - kb;
    let luma = |p: &[u8]| (kr * p[0] as f32 + kg * p[1] as f32 + kb * p[2] as f32) / 255.0;
    for y in 0..h {
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..];
            out[y * w + x] = (16.0 + 219.0 * luma(p)).round().clamp(0.0, 255.0) as u8;
        }
    }
    // Farbe aus dem Mittel jedes 2×2-Blocks.
    let uv = &mut out[w * h..];
    for by in 0..h / 2 {
        for bx in 0..w / 2 {
            let mut sum = [0.0f32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = &rgba[((by * 2 + dy) * w + bx * 2 + dx) * 4..];
                for k in 0..3 {
                    sum[k] += p[k] as f32 / 255.0 / 4.0;
                }
            }
            let yy = kr * sum[0] + kg * sum[1] + kb * sum[2];
            let cb = (sum[2] - yy) / (2.0 * (1.0 - kb));
            let cr = (sum[0] - yy) / (2.0 * (1.0 - kr));
            let i = by * w + bx * 2;
            uv[i] = (128.0 + 224.0 * cb).round().clamp(0.0, 255.0) as u8;
            uv[i + 1] = (128.0 + 224.0 * cr).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Zielgröße: wie bei Videos höchstens so groß, dass der größte Monitor gerade gefüllt wird; immer gerade.
pub fn target_size(w: u32, h: u32, opts: &DecodeOptions) -> (u32, u32) {
    let (tw, th) = opts.max_size.filter(|&(mw, mh)| mw > 0 && mh > 0).and_then(|m| scaled_size((w, h), m)).unwrap_or((w, h));
    // Ungerade Größen um ein Pixel kürzen (NV12 braucht 2×2-Blöcke); winzige Bilder auf 2 aufrunden.
    let even = |v: u32| if v < 2 { 2 } else { v & !1 };
    (even(tw), even(th))
}

/// Bild laden und als einzigen Frame liefern; danach warten, bis der Player verworfen wird.
#[cfg(windows)]
pub fn show(path: &Path, opts: DecodeOptions, tx: &SyncSender<Frame>, pool: &Receiver<Recycled>, stop: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let mut size = (0, 0);
    let (w, h, rgba) = crate::platform::sys::decode_image_file(path, |w, h| {
        size = target_size(w, h, &opts);
        size
    })?;
    // WIC liefert die erbetene Größe; nur bei Winzlingen (1 px) kann sie abweichen.
    let (w, h, rgba) = if (w, h) == size { (w, h, rgba) } else { pad_even(w, h, &rgba) };
    log::info!("Bild {path:?}: {w}×{h}");
    let frame = Frame {
        data: rgba_to_nv12(&rgba, w as usize, h as usize),
        gpu: None,
        pitch: w,
        width: w,
        height: h,
        y_rows: h,
        crop: [0.0, 0.0, 1.0, 1.0],
        pts: Duration::ZERO,
        bt709: true,
    };
    if tx.send(frame).is_err() {
        return Ok(());
    }
    // Kanal offen halten (sonst meldet der Player einen beendeten Dekoder); zurückgegebene Puffer verwerfen.
    while !stop.load(Ordering::Relaxed) {
        match pool.recv_timeout(Duration::from_millis(250)) {
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Auf gerade Breite und Höhe auffüllen (Randpixel wiederholen).
#[cfg_attr(not(windows), allow(dead_code))]
fn pad_even(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let (tw, th) = (w + w % 2, h + h % 2);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    for y in 0..th {
        for x in 0..tw {
            let s = ((y.min(h - 1) * w + x.min(w - 1)) * 4) as usize;
            let d = ((y * tw + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (tw, th, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_matches_shader_conversion() {
        // Reines Weiß, Schwarz, Rot und Blau als 2×2-Blöcke nebeneinander (8×2 Pixel).
        let colors = [[255u8, 255, 255, 255], [0, 0, 0, 255], [255, 0, 0, 255], [0, 0, 255, 255]];
        let (w, h) = (8usize, 2usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                rgba[(y * w + x) * 4..][..4].copy_from_slice(&colors[x / 2]);
            }
        }
        let nv = rgba_to_nv12(&rgba, w, h);
        // Rückweg wie im Shader (BT.709, begrenzter Wertebereich) muss die Farben treffen.
        for (i, c) in colors.iter().enumerate() {
            let y = (nv[i * 2] as f32 / 255.0 - 16.0 / 255.0) * (255.0 / 219.0);
            let cb = (nv[w * h + i * 2] as f32 / 255.0 - 0.5) * (255.0 / 224.0);
            let cr = (nv[w * h + i * 2 + 1] as f32 / 255.0 - 0.5) * (255.0 / 224.0);
            let rgb = [y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb];
            for k in 0..3 {
                assert!((rgb[k] * 255.0 - c[k] as f32).abs() < 3.0, "Farbe {i}, Kanal {k}: {:?} statt {:?}", rgb, c);
            }
        }
    }

    #[test]
    fn target_sizes() {
        let opts = DecodeOptions { max_size: Some((1920, 1080)), ..Default::default() };
        // 6000×4000-Foto auf 1080p: so klein, dass der Monitor gerade gefüllt ist.
        assert_eq!(target_size(6000, 4000, &opts), (1920, 1280));
        // Kleiner als der Monitor: nicht vergrößern, nur gerade machen.
        assert_eq!(target_size(1279, 721, &opts), (1278, 720));
        assert_eq!(target_size(1, 1, &DecodeOptions::default()), (2, 2));
        assert_eq!(pad_even(3, 1, &[9u8; 12]).0, 4);
    }
}
