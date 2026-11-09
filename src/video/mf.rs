//! Media Foundation: in Windows eingebauter, per Windows Update gepflegter Dekoder.
//! Hardware-Dekodierung (DXVA) über ein eigenes D3D11-Gerät, Ausgabe NV12, Endlosschleife.
//!
//! Zero-Copy (wenn der Renderer DX12 auf derselben GPU nutzt): Jedes Bild wird auf der GPU in einen Ring
//! geteilter NV12-Texturen kopiert, die der Renderer direkt liest. Sonst: Bild in den Arbeitsspeicher kopieren.

use super::{DecodeOptions, Frame, GpuFrame, Recycled, SharedRing, RING};
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::Duration;
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::VT_I8;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID};
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

pub fn decode_loop(path: &Path, opts: DecodeOptions, tx: &SyncSender<Frame>, pool: &Receiver<Recycled>, stop: &Arc<AtomicBool>) -> Result<()> {
    if !path.is_file() {
        bail!("Datei nicht gefunden");
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("MFStartup")?;
        let r = run(path, opts, tx, pool, stop);
        let _ = MFShutdown();
        r
    }
}

unsafe fn run(path: &Path, opts: DecodeOptions, tx: &SyncSender<Frame>, pool: &Receiver<Recycled>, stop: &Arc<AtomicBool>) -> Result<()> {
    let attrs = {
        let mut a = None;
        MFCreateAttributes(&mut a, 4)?;
        a.context("MFCreateAttributes")?
    };

    // Hardware-Dekodierung: D3D11-Gerät mit Video-Unterstützung an den Source Reader geben.
    let mut zero_copy: Option<ZeroCopy> = None;
    match create_dxgi_manager(opts) {
        Ok((mgr, device, gpu)) => {
            attrs.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, &mgr)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            if opts.gpu_luid.is_some() {
                match ZeroCopy::new(device) {
                    Ok(z) => zero_copy = Some(z),
                    Err(e) => log::warn!("Zero-Copy nicht möglich ({e:#}), Bilder gehen über den Arbeitsspeicher"),
                }
            }
            let mode = if zero_copy.is_some() { "Zero-Copy" } else { "über Arbeitsspeicher" };
            log::info!("Hardware-Dekodierung auf {gpu} ({mode}) für {path:?}");
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

    // Größer als der Monitor? Ohne Zero-Copy verkleinert der Videoprozessor auf der GPU – das spart vor
    // allem das teure Zurücklesen jedes Bildes in den Arbeitsspeicher (4K → 1080p = ¼ der Daten).
    // Mit Zero-Copy entfällt das Zurücklesen; dann ist es günstiger, der Shader verkleinert beim Zeichnen
    // (gemessen: 3D-Last RTX 4070 18 % → 11 %, AMD-iGPU 57 % → 44 %).
    let scale_to = if zero_copy.is_none() { opts.max_size } else { None };
    if let Some((w, h)) = scale_to.and_then(|m| super::scaled_size((fmt.width, fmt.height), m)) {
        let small = MFCreateMediaType()?;
        small.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        small.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        small.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        match reader.SetCurrentMediaType(FIRST_VIDEO, None, &small) {
            Ok(()) => {
                fmt = read_format(&reader)?;
                log::info!("{path:?}: wird auf der GPU auf {}x{} verkleinert", fmt.width, fmt.height);
            }
            Err(e) => {
                log::warn!("{path:?}: Verkleinern nicht möglich ({e}), dekodiere in voller Größe");
                reader.SetCurrentMediaType(FIRST_VIDEO, None, &want)?;
                fmt = read_format(&reader)?;
            }
        }
    }

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

        let frame = match zero_copy.as_mut().map(|z| z.push(&sample, &fmt, pts, pool, stop)) {
            Some(Ok(Some(f))) => f,
            Some(Ok(None)) => return Ok(()), // beendet, während auf einen freien Slot gewartet wurde
            other => {
                if let Some(Err(e)) = other {
                    log::warn!("Zero-Copy abgeschaltet ({e:#}), Bilder gehen ab jetzt über den Arbeitsspeicher");
                    zero_copy = None;
                }
                // Arbeitsspeicher-Weg: einen zurückgegebenen Puffer wiederverwenden.
                let buf = std::iter::from_fn(|| pool.try_recv().ok())
                    .find_map(|r| match r {
                        Recycled::Buffer(b) => Some(b),
                        Recycled::Slot(_) => None,
                    })
                    .unwrap_or_default();
                copy_sample(&sample, &fmt, buf, pts)?
            }
        };

        // Blockiert ohne CPU-Last, solange die Warteschlange voll ist (z. B. pausiert).
        // Wird der Player freigegeben, schlägt send fehl und der Thread endet.
        if tx.send(frame).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// D3D11-Gerät für die Hardware-Dekodierung – auf der GPU des Renderers (Zero-Copy: exakt dieselbe, per LUID),
/// sonst auf derselben GPU-Sorte (`gpu = "low"/"high"`). Ohne das landet der Dekoder auf der Standard-GPU,
/// und jedes Bild wandert zusätzlich zwischen zwei Grafikkarten.
unsafe fn create_dxgi_manager(opts: DecodeOptions) -> Result<(IMFDXGIDeviceManager, ID3D11Device, String)> {
    use windows::Win32::Graphics::Dxgi::*;
    let pref = if opts.low_power { DXGI_GPU_PREFERENCE_MINIMUM_POWER } else { DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE };
    let factory = CreateDXGIFactory1::<IDXGIFactory6>().ok();
    let adapter: Option<IDXGIAdapter1> = factory.as_ref().and_then(|f| {
        let by_luid = opts.gpu_luid.and_then(|l| f.EnumAdapterByLuid::<IDXGIAdapter1>(LUID { LowPart: l as u32, HighPart: (l >> 32) as i32 }).ok());
        by_luid.or_else(|| f.EnumAdapterByGpuPreference::<IDXGIAdapter1>(0, pref).ok())
    });
    let name = adapter
        .as_ref()
        .and_then(|a| a.GetDesc1().ok())
        .map(|d| String::from_utf16_lossy(&d.Description[..d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len())]))
        .unwrap_or_else(|| "Standard-GPU".into());
    let mut device: Option<ID3D11Device> = None;
    D3D11CreateDevice(
        adapter.as_ref().map(|a| a.cast::<IDXGIAdapter>()).transpose()?.as_ref(),
        // Mit vorgegebenem Adapter muss der Treibertyp UNKNOWN sein.
        if adapter.is_some() { windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
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
    Ok((mgr, device, name))
}

// ───────────────────────────── Zero-Copy ─────────────────────────────

/// Kopiert dekodierte Bilder auf der GPU in einen Ring geteilter Texturen.
struct ZeroCopy {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext4,
    /// Media Foundation nutzt dasselbe Gerät aus eigenen Threads.
    mt: ID3D11Multithread,
    ready: ID3D11Fence,
    released: ID3D11Fence,
    ready_value: u64,
    ring: Option<Arc<SharedRing>>,
    textures: Vec<ID3D11Texture2D>,
    /// Freie Slots mit dem `released`-Wert, ab dem sie überschrieben werden dürfen.
    free: Vec<(usize, u64)>,
}

static RING_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl ZeroCopy {
    unsafe fn new(device: ID3D11Device) -> Result<Self> {
        let d5: ID3D11Device5 = device.cast().context("D3D11.4 (ID3D11Device5) fehlt")?;
        let fence = || -> Result<ID3D11Fence> {
            let mut f = None;
            d5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut f)?;
            f.context("CreateFence")
        };
        let (ready, released) = (fence()?, fence()?);
        let ctx: ID3D11DeviceContext4 = device.GetImmediateContext()?.cast().context("ID3D11DeviceContext4 fehlt")?;
        let mt: ID3D11Multithread = device.cast()?;
        Ok(Self { device, ctx, mt, ready, released, ready_value: 0, ring: None, textures: Vec::new(), free: Vec::new() })
    }

    /// Ring passend zur Bildgröße (neu) anlegen.
    unsafe fn ensure_ring(&mut self, width: u32, height: u32) -> Result<Arc<SharedRing>> {
        if let Some(r) = self.ring.as_ref().filter(|r| r.width == width && r.height == height) {
            return Ok(r.clone());
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        let mut textures = Vec::with_capacity(RING);
        let mut handles = Vec::with_capacity(RING);
        for _ in 0..RING {
            let mut t = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut t)).context("geteilte NV12-Textur anlegen")?;
            let t = t.context("CreateTexture2D")?;
            let res: windows::Win32::Graphics::Dxgi::IDXGIResource1 = t.cast()?;
            let access = windows::Win32::Graphics::Dxgi::DXGI_SHARED_RESOURCE_READ | windows::Win32::Graphics::Dxgi::DXGI_SHARED_RESOURCE_WRITE;
            handles.push(res.CreateSharedHandle(None, access.0, windows::core::PCWSTR::null())?.0 as isize);
            textures.push(t);
        }
        let ready = self.ready.CreateSharedHandle(None, GENERIC_ALL.0, windows::core::PCWSTR::null())?.0 as isize;
        let released = self.released.CreateSharedHandle(None, GENERIC_ALL.0, windows::core::PCWSTR::null())?.0 as isize;
        let ring = Arc::new(SharedRing {
            id: RING_ID.fetch_add(1, Ordering::Relaxed),
            textures: handles,
            ready_fence: ready,
            released_fence: released,
            width,
            height,
        });
        // Neue Texturen: alle Slots frei. Der `released`-Wert läuft über Ringe hinweg weiter.
        self.free = (0..RING).map(|i| (i, 0)).collect();
        self.textures = textures;
        self.ring = Some(ring.clone());
        Ok(ring)
    }

    /// Bild in einen freien Slot kopieren. `Ok(None)` = beendet, während auf einen Slot gewartet wurde.
    unsafe fn push(&mut self, sample: &IMFSample, fmt: &Format, pts: Duration, pool: &Receiver<Recycled>, stop: &AtomicBool) -> Result<Option<Frame>> {
        let buffer = sample.GetBufferByIndex(0)?;
        let dxgi: IMFDXGIBuffer = buffer.cast().context("Bild liegt nicht auf der GPU")?;
        let mut raw = std::ptr::null_mut();
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)?;
        let src = ID3D11Texture2D::from_raw(raw);
        let sub = dxgi.GetSubresourceIndex()?;
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        src.GetDesc(&mut sd);
        if sd.Format != windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12 {
            bail!("Dekoder liefert {:?} statt NV12", sd.Format);
        }
        // NV12 braucht gerade Maße; nie mehr kopieren, als die Quelle hat.
        let w = fmt.width.min(sd.Width) & !1;
        let h = fmt.height.min(sd.Height) & !1;
        let ring = self.ensure_ring(w, h)?;

        // Freien Slot holen – blockiert ohne CPU-Last, bis der Renderer einen zurückgibt.
        let (slot, after) = loop {
            while let Ok(r) = pool.try_recv() {
                self.take_back(r, ring.id);
            }
            if let Some(s) = self.free.pop() {
                break s;
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(None);
            }
            if let Ok(r) = pool.recv_timeout(Duration::from_millis(100)) {
                self.take_back(r, ring.id);
            }
        };

        self.mt.Enter();
        let r = (|| -> Result<()> {
            if after > 0 {
                // GPU wartet, bis der Renderer mit diesem Slot fertig ist (blockiert nicht die CPU).
                self.ctx.Wait(&self.released, after)?;
            }
            let bx = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
            self.ctx.CopySubresourceRegion(&self.textures[slot], 0, 0, 0, 0, &src, sub, Some(&bx));
            self.ready_value += 1;
            self.ctx.Signal(&self.ready, self.ready_value)?;
            self.ctx.Flush();
            Ok(())
        })();
        self.mt.Leave();
        if let Err(e) = r {
            self.free.push((slot, after));
            return Err(e);
        }
        let crop = [fmt.crop[0] * fmt.width as f32 / w as f32, fmt.crop[1] * fmt.height as f32 / h as f32, fmt.crop[2] * fmt.width as f32 / w as f32, fmt.crop[3] * fmt.height as f32 / h as f32];
        Ok(Some(Frame {
            data: Vec::new(),
            gpu: Some(GpuFrame { ring, slot, ready_value: self.ready_value }),
            pitch: 0,
            width: w,
            height: h,
            y_rows: h,
            crop: crop.map(|c| c.min(1.0)),
            pts,
            bt709: fmt.bt709,
        }))
    }

    fn take_back(&mut self, r: Recycled, ring: u64) {
        if let Recycled::Slot(s) = r {
            if s.ring == ring && !self.free.iter().any(|(i, _)| *i == s.slot) {
                self.free.push((s.slot, s.after));
            }
        }
    }
}

impl Drop for SharedRing {
    fn drop(&mut self) {
        unsafe {
            for &h in self.textures.iter().chain([&self.ready_fence, &self.released_fence]) {
                let _ = CloseHandle(HANDLE(h as *mut _));
            }
        }
    }
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
        return Ok(Frame { data, gpu: None, pitch, width: fmt.width, height: fmt.height, y_rows, crop: fmt.crop, pts, bt709: fmt.bt709 });
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
    Ok(Frame { data, gpu: None, pitch: pitch as u32, width: fmt.width, height: fmt.height, y_rows: fmt.height, crop: fmt.crop, pts, bt709: fmt.bt709 })
}

// ───────────────────────────── Vorschaubild ─────────────────────────────

/// Vorschaubild (ein Bild bei ~2 s) als RGBA, höchstens `max_w` Pixel breit. Software-Dekodierung,
/// läuft im Einstellungsfenster in einem Hintergrund-Thread.
pub fn thumbnail(path: &Path, max_w: u32) -> Result<(u32, u32, Vec<u8>)> {
    if !path.is_file() {
        bail!("Datei nicht gefunden");
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).context("MFStartup")?;
        let r = thumbnail_inner(path, max_w.max(1));
        let _ = MFShutdown();
        r
    }
}

unsafe fn thumbnail_inner(path: &Path, max_w: u32) -> Result<(u32, u32, Vec<u8>)> {
    let attrs = {
        let mut a = None;
        MFCreateAttributes(&mut a, 1)?;
        a.context("MFCreateAttributes")?
    };
    // Lässt Media Foundation selbst nach RGB32 konvertieren.
    attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
    let reader = MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), &attrs).context("Video öffnen")?;
    reader.SetStreamSelection(ALL_STREAMS, false)?;
    reader.SetStreamSelection(FIRST_VIDEO, true)?;
    let want = MFCreateMediaType()?;
    want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
    reader.SetCurrentMediaType(FIRST_VIDEO, None, &want).context("kein RGB32")?;

    let mt = reader.GetCurrentMediaType(FIRST_VIDEO)?;
    let size = mt.GetUINT64(&MF_MT_FRAME_SIZE)?;
    let (w, h) = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
    if w == 0 || h == 0 {
        bail!("ungültige Bildgröße");
    }
    let default_stride = mt.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).unwrap_or(w as i32 * 4);

    // Etwas hinter den Anfang springen – das erste Bild ist oft schwarz. Kurze Videos: von vorn.
    for seek in [20_000_000i64, 0] {
        let mut pos = PROPVARIANT::default();
        (*pos.Anonymous.Anonymous).vt = VT_I8;
        (*pos.Anonymous.Anonymous).Anonymous.hVal = seek;
        let _ = reader.SetCurrentPosition(&windows::core::GUID::zeroed(), &pos);
        for _ in 0..60 {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader.ReadSample(FIRST_VIDEO, 0, None, Some(&mut flags), None, Some(&mut sample))?;
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            if let Some(sample) = sample {
                return rgb32_to_thumb(&sample, w, h, default_stride, max_w);
            }
        }
    }
    bail!("kein Bild gefunden")
}

unsafe fn rgb32_to_thumb(sample: &IMFSample, w: usize, h: usize, default_stride: i32, max_w: u32) -> Result<(u32, u32, Vec<u8>)> {
    let buffer = sample.ConvertToContiguousBuffer()?;
    let (base, pitch, unlock2d) = if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        b2.Lock2D(&mut scan0, &mut pitch)?;
        (scan0 as *const u8, pitch as isize, Some(b2))
    } else {
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len))?;
        if (len as usize) < default_stride.unsigned_abs() as usize * h {
            let _ = buffer.Unlock();
            bail!("Puffer zu klein");
        }
        // Negativer Stride = Bild steht auf dem Kopf (bottom-up).
        let base = if default_stride < 0 { ptr.add(default_stride.unsigned_abs() as usize * (h - 1)) } else { ptr };
        (base as *const u8, default_stride as isize, None)
    };

    // Box-Filter: jedes Zielpixel mittelt seinen Quellbereich (sauberer als „nächster Nachbar“).
    let tw = (max_w as usize).min(w);
    let th = (h * tw / w).max(1);
    let mut out = vec![0u8; tw * th * 4];
    for ty in 0..th {
        let (y0, y1) = (ty * h / th, ((ty + 1) * h / th).max(ty * h / th + 1));
        for tx in 0..tw {
            let (x0, x1) = (tx * w / tw, ((tx + 1) * w / tw).max(tx * w / tw + 1));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            // Bei großen Bereichen nur jede zweite Zeile/Spalte – reicht für ein Vorschaubild.
            let step = if (x1 - x0) * (y1 - y0) > 64 { 2 } else { 1 };
            for y in (y0..y1).step_by(step) {
                let row = base.offset(y as isize * pitch);
                for x in (x0..x1).step_by(step) {
                    let p = row.add(x * 4); // B G R X
                    b += *p as u32;
                    g += *p.add(1) as u32;
                    r += *p.add(2) as u32;
                    n += 1;
                }
            }
            let o = (ty * tw + tx) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = 255;
        }
    }
    match unlock2d {
        Some(b2) => {
            let _ = b2.Unlock2D();
        }
        None => {
            let _ = buffer.Unlock();
        }
    }
    Ok((tw as u32, th as u32, out))
}
