//! Media Foundation: in Windows eingebauter, per Windows Update gepflegter Dekoder.
//! Hardware-Dekodierung (DXVA) über ein eigenes D3D11-Gerät, Ausgabe NV12, Endlosschleife.

use super::Frame;
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

const FIRST_VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const ALL_STREAMS: u32 = MF_SOURCE_READER_ALL_STREAMS.0 as u32;

struct Format {
    width: u32,
    height: u32,
    default_stride: Option<u32>,
    crop: [f32; 4],
    frame_dur: Duration,
    bt709: bool,
}

pub fn decode_loop(path: &Path, tx: &SyncSender<Frame>, pool: &Receiver<Vec<u8>>, stop: &Arc<AtomicBool>) -> Result<()> {
    if !path.is_file() {
        bail!("Datei nicht gefunden");
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("MFStartup")?;
        let r = run(path, tx, pool, stop);
        let _ = MFShutdown();
        r
    }
}

unsafe fn run(path: &Path, tx: &SyncSender<Frame>, pool: &Receiver<Vec<u8>>, stop: &Arc<AtomicBool>) -> Result<()> {
    let attrs = {
        let mut a = None;
        MFCreateAttributes(&mut a, 4)?;
        a.context("MFCreateAttributes")?
    };

    // Hardware-Dekodierung: D3D11-Gerät mit Video-Unterstützung an den Source Reader geben.
    match create_dxgi_manager() {
        Ok(mgr) => {
            attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &mgr)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            log::info!("Hardware-Dekodierung aktiv für {path:?}");
        }
        Err(e) => log::warn!("Keine Hardware-Dekodierung ({e}), nutze Software-Dekoder"),
    }
    // Erlaubt Formatkonvertierung (z. B. 10-Bit P010 → NV12) auf der GPU.
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)?;

    let url = HSTRING::from(path.as_os_str());
    let reader = MFCreateSourceReaderFromURL(&url, &attrs).context("Video öffnen")?;

    // Nur den Videostream lesen – Ton wird nie dekodiert.
    reader.SetStreamSelection(ALL_STREAMS, false)?;
    reader.SetStreamSelection(FIRST_VIDEO, true)?;

    let want = MFCreateMediaType()?;
    want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    reader.SetCurrentMediaType(FIRST_VIDEO, None, &want).context("Video kann nicht als NV12 dekodiert werden")?;

    let mut fmt = read_format(&reader)?;
    log::info!("{path:?}: {}x{}, {:.2} fps", fmt.width, fmt.height, 1.0 / fmt.frame_dur.as_secs_f64());

    let mut loop_base = Duration::ZERO;
    let mut last_pts = Duration::ZERO;
    let mut frames_in_pass = 0u64;

    while !stop.load(Ordering::Relaxed) {
        let mut flags = 0u32;
        let mut ts = 0i64;
        let mut sample: Option<IMFSample> = None;
        reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;

        if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
            fmt = read_format(&reader)?;
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            if frames_in_pass == 0 {
                bail!("Video enthält keine Bilder");
            }
            // Endlosschleife: zurück an den Anfang, Zeitstempel laufen weiter.
            loop_base = last_pts + fmt.frame_dur;
            frames_in_pass = 0;
            let mut pos = PROPVARIANT::default(); // VT_I8 = 0 → Anfang
            (*pos.Anonymous.Anonymous).vt = VT_I8;
            (*pos.Anonymous.Anonymous).Anonymous.hVal = 0;
            reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &pos)?;
            continue;
        }
        let Some(sample) = sample else { continue };
        let pts = loop_base + Duration::from_nanos(ts.max(0) as u64 * 100);
        last_pts = pts;
        frames_in_pass += 1;

        let buf = pool.try_recv().unwrap_or_default();
        let frame = copy_sample(&sample, &fmt, buf, pts)?;

        // Blockiert ohne CPU-Last, solange die Warteschlange voll ist (z. B. pausiert).
        // Wird der Player freigegeben, schlägt send fehl und der Thread endet.
        if tx.send(frame).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

unsafe fn create_dxgi_manager() -> Result<IMFDXGIDeviceManager> {
    let mut device: Option<ID3D11Device> = None;
    D3D11CreateDevice(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        None,
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        None,
    )?;
    let device = device.context("kein D3D11-Gerät")?;
    // Media Foundation greift aus mehreren Threads zu.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        let _ = mt.SetMultithreadProtected(true);
    }
    let mut token = 0u32;
    let mut mgr: Option<IMFDXGIDeviceManager> = None;
    MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
    let mgr = mgr.context("kein DXGI-Manager")?;
    mgr.ResetDevice(&device, token)?;
    Ok(mgr)
}

unsafe fn read_format(reader: &IMFSourceReader) -> Result<Format> {
    let mt = reader.GetCurrentMediaType(FIRST_VIDEO)?;
    let size = mt.GetUINT64(&MF_MT_FRAME_SIZE)?;
    let (width, height) = ((size >> 32) as u32, (size & 0xFFFF_FFFF) as u32);
    if width == 0 || height == 0 {
        bail!("ungültige Bildgröße");
    }
    let frame_dur = mt
        .GetUINT64(&MF_MT_FRAME_RATE)
        .ok()
        .map(|r| ((r >> 32) as f64, (r & 0xFFFF_FFFF) as f64))
        .filter(|(n, d)| *n > 0.0 && *d > 0.0)
        .map(|(n, d)| Duration::from_secs_f64(d / n))
        .unwrap_or(Duration::from_millis(33));
    let default_stride = mt.GetUINT32(&MF_MT_DEFAULT_STRIDE).ok().map(|s| (s as i32).unsigned_abs());

    // Sichtbarer Bereich (z. B. 1920x1080 innerhalb von 1920x1088).
    let mut crop = [0.0, 0.0, 1.0, 1.0];
    let mut area = MFVideoArea::default();
    let blob = std::slice::from_raw_parts_mut(&mut area as *mut _ as *mut u8, std::mem::size_of::<MFVideoArea>());
    if mt.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
        let ox = area.OffsetX.value as f32 + area.OffsetX.fract as f32 / 65536.0;
        let oy = area.OffsetY.value as f32 + area.OffsetY.fract as f32 / 65536.0;
        crop = [ox / width as f32, oy / height as f32, area.Area.cx as f32 / width as f32, area.Area.cy as f32 / height as f32];
    }

    let bt709 = match mt.GetUINT32(&MF_MT_YUV_MATRIX) {
        Ok(m) if m == MFVideoTransferMatrix_BT601.0 as u32 => false,
        Ok(m) if m == MFVideoTransferMatrix_BT709.0 as u32 => true,
        _ => height >= 720,
    };
    Ok(Format { width, height, default_stride, crop, frame_dur, bt709 })
}

unsafe fn copy_sample(sample: &IMFSample, fmt: &Format, mut data: Vec<u8>, pts: Duration) -> Result<Frame> {
    let buffer = sample.ConvertToContiguousBuffer()?;
    let uv_rows = fmt.height.div_ceil(2) as usize;

    // Bevorzugt 2D-Sperre: liefert die echte Zeilenlänge (Pitch) der GPU-Oberfläche.
    if let Ok(b2) = buffer.cast::<IMF2DBuffer2>() {
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        let mut start = std::ptr::null_mut();
        let mut len = 0u32;
        b2.Lock2DSize(MF2DBuffer_LockFlags_Read, &mut scan0, &mut pitch, &mut start, &mut len)?;
        let result = (|| {
            if pitch <= 0 {
                bail!("negativer Pitch wird nicht unterstützt");
            }
            let pitch = pitch as usize;
            // Bei manchen Treibern ist die Y-Ebene höher als das Bild (Ausrichtung).
            let rows_total = len as usize / pitch;
            let y_rows = (rows_total * 2 / 3).max(fmt.height as usize);
            let needed = pitch * (y_rows + uv_rows);
            let available = len as usize - (scan0 as usize - start as usize);
            if needed > available {
                bail!("Puffer zu klein ({available} < {needed})");
            }
            data.clear();
            data.extend_from_slice(std::slice::from_raw_parts(scan0, needed));
            Ok((pitch as u32, y_rows as u32))
        })();
        let _ = b2.Unlock2D();
        let (pitch, y_rows) = result?;
        return Ok(Frame { data, pitch, width: fmt.width, height: fmt.height, y_rows, crop: fmt.crop, pts, bt709: fmt.bt709 });
    }

    // Fallback: zusammenhängender Speicher, Pitch aus dem Medientyp.
    let mut ptr = std::ptr::null_mut();
    let mut len = 0u32;
    buffer.Lock(&mut ptr, None, Some(&mut len))?;
    let pitch = fmt.default_stride.unwrap_or(fmt.width) as usize;
    let needed = pitch * (fmt.height as usize + uv_rows);
    let ok = needed <= len as usize;
    if ok {
        data.clear();
        data.extend_from_slice(std::slice::from_raw_parts(ptr, needed));
    }
    let _ = buffer.Unlock();
    if !ok {
        bail!("Puffer zu klein ({len} < {needed})");
    }
    Ok(Frame { data, pitch: pitch as u32, width: fmt.width, height: fmt.height, y_rows: fmt.height, crop: fmt.crop, pts, bt709: fmt.bt709 })
}
