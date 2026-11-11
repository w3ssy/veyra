//! GPU-Ausgabe mit wgpu: ein Gerät für alle Monitore, eine Oberfläche pro Monitor,
//! Videotexturen pro Datei (werden von allen Monitoren mit demselben Video geteilt).

use crate::audio::Levels;
use crate::config::{Backend, Effect, Fit, GpuPreference};
use crate::video::{Frame, SlotRelease};
use anyhow::{Context, Result};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use wgpu::rwh::{RawDisplayHandle, RawWindowHandle};

pub struct Gpu {
    instance: wgpu::Instance,
    power: wgpu::PowerPreference,
    /// Wird erst mit der ersten Fensteroberfläche angelegt, damit garantiert eine
    /// Grafikkarte/ein Backend gewählt wird, das auf dieses Fenster zeichnen kann.
    dev: Option<Dev>,
}

struct Dev {
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    format: wgpu::TextureFormat,
    /// Widgets: fertige Bilder mit Alpha über das Video blenden.
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_layout: wgpu::BindGroupLayout,
    /// DX12 mit NV12-Texturen → geteilte Texturen des Dekoders direkt lesbar (Zero-Copy).
    nv12: bool,
}

/// Das aktuelle Bild eines Videos, als Y- und UV-Ebene.
pub struct VideoTex {
    planes: Planes,
    width: u32,
    height: u32,
    crop: [f32; 4],
    bt709: bool,
}

// Es gibt genau ein `Planes` pro Video – der Größenunterschied der Varianten ist bedeutungslos.
#[allow(clippy::large_enum_variant)]
enum Planes {
    /// Eigene Texturen, Bilder werden hochgeladen.
    Uploaded { y: wgpu::Texture, uv: wgpu::Texture, y_view: wgpu::TextureView, uv_view: wgpu::TextureView, generation: u64 },
    /// Zero-Copy: geteilte Texturen des Dekoders, `current` = angezeigter Slot.
    Shared(SharedTex),
}

#[cfg(windows)]
use windows::Win32::Graphics::Direct3D12::ID3D12Fence;

struct SharedTex {
    ring: u64,
    /// Pro Slot: Ebenen-Ansichten (die Textur lebt in den Ansichten weiter) und eine Kennung für den Bind-Group-Cache.
    slots: Vec<(wgpu::TextureView, wgpu::TextureView, u64)>,
    current: Option<usize>,
    #[cfg(windows)]
    ready: ID3D12Fence,
    #[cfg(windows)]
    released: ID3D12Fence,
    /// Zuletzt nach dem Zeichnen signalisierter `released`-Wert.
    render_value: Cell<u64>,
}

impl VideoTex {
    fn views(&self) -> Option<(&wgpu::TextureView, &wgpu::TextureView, u64)> {
        match &self.planes {
            Planes::Uploaded { y_view, uv_view, generation, .. } => Some((y_view, uv_view, *generation)),
            Planes::Shared(s) => s.current.map(|c| (&s.slots[c].0, &s.slots[c].1, s.slots[c].2)),
        }
    }

    /// Den angezeigten Ring-Slot abgeben (wenn das Video wechselt oder auf den Arbeitsspeicher-Weg zurückfällt).
    fn release_current(&self) -> Option<SlotRelease> {
        match &self.planes {
            Planes::Shared(s) => s.current.map(|slot| SlotRelease { ring: s.ring, slot, after: s.render_value.get() }),
            Planes::Uploaded { .. } => None,
        }
    }
}

/// Eine Fensteroberfläche (ein Monitor).
pub struct Target {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    uniforms: wgpu::Buffer,
    /// Bind Groups je Textur(-Slot); beim Zero-Copy-Ring wechseln sie jedes Bild, daher ein kleiner Cache.
    binds: Vec<(u64, wgpu::BindGroup)>,
}

/// Ein Widget-Bild auf einer Oberfläche (Textur mit vormultipliziertem Alpha plus Position) und
/// eine gleich große Leucht-Ebene, die der Shader mit `flash` dazumischt (Countdown-Effekt).
pub struct Overlay {
    texture: wgpu::Texture,
    glow: wgpu::Texture,
    has_glow: bool,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
    size: (u32, u32),
    flash: Cell<f32>,
}

pub struct DrawParams {
    pub fit: Fit,
    pub effect: Effect,
    pub intensity: f32,
    pub levels: Levels,
    pub time: f32,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);

impl Gpu {
    pub fn new(backend: Backend, power: GpuPreference) -> Self {
        let backends = match backend {
            // DX12 ist unter Windows der native, zuverlässige Weg; Vulkan nur auf ausdrücklichen Wunsch.
            Backend::Auto if cfg!(windows) => wgpu::Backends::DX12,
            Backend::Auto => wgpu::Backends::VULKAN,
            Backend::Dx12 => wgpu::Backends::DX12,
            Backend::Vulkan => wgpu::Backends::VULKAN,
        };
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = backends;
        let instance = wgpu::Instance::new(desc);
        let power = match power {
            GpuPreference::Low => wgpu::PowerPreference::LowPower,
            GpuPreference::High => wgpu::PowerPreference::HighPerformance,
        };
        Self { instance, power, dev: None }
    }

    fn dev(&self) -> &Dev {
        self.dev.as_ref().expect("GPU-Gerät noch nicht angelegt")
    }
}

impl Dev {
    fn new(instance: &wgpu::Instance, power: wgpu::PowerPreference, surface: &wgpu::Surface) -> Result<Self> {
        for a in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())) {
            let i = a.get_info();
            let ok = !surface.get_capabilities(&a).formats.is_empty();
            log::info!("Grafikkarte: {} ({:?}, {:?}) – kann auf Wallpaper-Fenster zeichnen: {ok}", i.name, i.backend, i.device_type);
        }
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: power,
            force_fallback_adapter: false,
            compatible_surface: Some(surface),
            ..Default::default()
        }))
        .context("keine Grafikkarte gefunden, die auf das Wallpaper-Fenster zeichnen kann")?;
        let info = adapter.get_info();
        log::info!("GPU gewählt: {} ({:?})", info.name, info.backend);

        // NV12-Texturen (nur DX12/Vulkan) erlauben, geteilte Texturen des Dekoders direkt zu lesen.
        let nv12 = info.backend == wgpu::Backend::Dx12 && adapter.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("veyra"),
            required_features: if nv12 { wgpu::Features::TEXTURE_FORMAT_NV12 } else { wgpu::Features::empty() },
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .context("GPU-Gerät anlegen")?;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("veyra"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        // Video-Werte sind bereits gammakodiert → in ein Nicht-sRGB-Format schreiben.
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("veyra"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let overlay_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("veyra-overlay"),
            source: wgpu::ShaderSource::Wgsl(include_str!("overlay.wgsl").into()),
        });
        let overlay_tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let overlay_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                overlay_tex(1),
                overlay_tex(2),
            ],
        });
        let overlay_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&overlay_layout)],
            ..Default::default()
        });
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("veyra-overlay"),
            layout: Some(&overlay_pl),
            vertex: wgpu::VertexState {
                module: &overlay_shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &overlay_shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self { adapter, device, queue, pipeline, layout, sampler, format, overlay_pipeline, overlay_layout, nv12 })
    }
}

impl Gpu {

    /// Oberfläche für ein natives Fenster anlegen.
    ///
    /// # Safety
    /// Das Fenster muss länger leben als das zurückgegebene `Target`.
    pub unsafe fn create_target(&mut self, display: RawDisplayHandle, window: RawWindowHandle, width: u32, height: u32) -> Result<Target> {
        let surface = self
            .instance
            .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle { raw_display_handle: Some(display), raw_window_handle: window })
            .context("Oberfläche anlegen")?;
        if self.dev.is_none() {
            self.dev = Some(Dev::new(&self.instance, self.power, &surface)?);
        }
        let d = self.dev();
        let caps = surface.get_capabilities(&d.adapter);
        if !caps.formats.contains(&d.format) {
            anyhow::bail!(
                "Grafikkarte {} kann nicht auf dieses Fenster zeichnen (Formate: {:?})",
                d.adapter.get_info().name,
                caps.formats
            );
        }
        let alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes.first().copied().unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: d.format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 1,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&d.device, &config);
        let uniforms = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Target { surface, config, uniforms, binds: Vec::new() })
    }

    /// Grafikkarte des Renderers (LUID) – nur wenn er geteilte NV12-Texturen öffnen kann (DX12).
    /// Der Dekoder läuft dann auf genau dieser GPU und liefert Bilder ohne Umweg über den Arbeitsspeicher.
    pub fn zero_copy_luid(&self) -> Option<u64> {
        #[cfg(windows)]
        {
            let d = self.dev.as_ref().filter(|d| d.nv12)?;
            // SAFETY: nur lesender Zugriff auf das rohe Gerät.
            let hal = unsafe { d.device.as_hal::<wgpu::hal::api::Dx12>() }?;
            let l = unsafe { hal.raw_device().GetAdapterLuid() };
            Some(((l.HighPart as u32 as u64) << 32) | l.LowPart as u64)
        }
        #[cfg(not(windows))]
        None
    }

    /// Neues Bild übernehmen. Zero-Copy: nur den Slot umschalten; sonst Pixel hochladen.
    /// Liefert den vorher angezeigten Ring-Slot zurück, den der Dekoder wieder füllen darf.
    pub fn upload(&self, tex: &mut Option<VideoTex>, f: &Frame) -> Result<Option<SlotRelease>> {
        #[cfg(windows)]
        if let Some(g) = &f.gpu {
            return self.show_shared(tex, f, g);
        }
        let d = self.dev();
        let fits = tex.as_ref().is_some_and(|t| matches!(t.planes, Planes::Uploaded { .. }) && t.width == f.width && t.height == f.height);
        let mut released = None;
        if !fits {
            released = tex.as_ref().and_then(VideoTex::release_current);
            *tex = Some(d.create_video_tex(f.width, f.height));
        }
        let t = tex.as_mut().unwrap();
        t.crop = f.crop;
        t.bt709 = f.bt709;
        let Planes::Uploaded { y, uv, .. } = &t.planes else { unreachable!() };
        let uv_w = f.width.div_ceil(2);
        let uv_h = f.height.div_ceil(2);
        d.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: y, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &f.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(f.pitch), rows_per_image: None },
            wgpu::Extent3d { width: f.width, height: f.height, depth_or_array_layers: 1 },
        );
        d.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: uv, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &f.data,
            wgpu::TexelCopyBufferLayout { offset: f.uv_offset() as u64, bytes_per_row: Some(f.pitch), rows_per_image: None },
            wgpu::Extent3d { width: uv_w, height: uv_h, depth_or_array_layers: 1 },
        );
        Ok(released)
    }

    /// Zero-Copy: Slot des neuen Bildes anzeigen, vorher die GPU auf das Fertig-Signal des Dekoders warten lassen.
    #[cfg(windows)]
    fn show_shared(&self, tex: &mut Option<VideoTex>, f: &Frame, g: &crate::video::GpuFrame) -> Result<Option<SlotRelease>> {
        let d = self.dev();
        let mut released = None;
        let same_ring = tex.as_ref().is_some_and(|t| matches!(&t.planes, Planes::Shared(s) if s.ring == g.ring.id));
        if !same_ring {
            released = tex.as_ref().and_then(VideoTex::release_current);
            *tex = Some(d.import_ring(&g.ring)?);
        }
        let t = tex.as_mut().unwrap();
        t.crop = f.crop;
        t.bt709 = f.bt709;
        let Planes::Shared(s) = &mut t.planes else { unreachable!() };
        // SAFETY: die Fence stammt vom selben Gerät; die Wartebedingung gilt für die nächste Übergabe.
        let q = unsafe { d.queue.as_hal::<wgpu::hal::api::Dx12>() }.context("DX12-Queue nicht verfügbar")?;
        q.add_wait_fence(s.ready.clone(), g.ready_value);
        let previous = s.current.replace(g.slot);
        if released.is_none() {
            released = previous.map(|slot| SlotRelease { ring: s.ring, slot, after: s.render_value.get() });
        }
        Ok(released)
    }

}

impl Dev {
    fn create_video_tex(&self, width: u32, height: u32) -> VideoTex {
        let mk = |w, h, format| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = mk(width, height, wgpu::TextureFormat::R8Unorm);
        let uv = mk(width.div_ceil(2), height.div_ceil(2), wgpu::TextureFormat::Rg8Unorm);
        VideoTex {
            planes: Planes::Uploaded {
                y_view: y.create_view(&Default::default()),
                uv_view: uv.create_view(&Default::default()),
                y,
                uv,
                generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            },
            width,
            height,
            crop: [0.0, 0.0, 1.0, 1.0],
            bt709: true,
        }
    }

    /// Geteilte Texturen und Fences des Dekoders in DX12 öffnen und als wgpu-Texturen einbinden.
    #[cfg(windows)]
    fn import_ring(&self, ring: &crate::video::SharedRing) -> Result<VideoTex> {
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::Graphics::Direct3D12::ID3D12Resource;
        let (resources, ready, released) = {
            // SAFETY: rohes Gerät nur zum Öffnen der Handles; die Sperre endet mit diesem Block.
            let hal = unsafe { self.device.as_hal::<wgpu::hal::api::Dx12>() }.context("kein DX12-Gerät")?;
            let raw = hal.raw_device();
            let open_res = |h: isize| -> Result<ID3D12Resource> {
                let mut r = None;
                unsafe { raw.OpenSharedHandle(HANDLE(h as *mut _), &mut r) }.context("geteilte Textur öffnen")?;
                r.context("OpenSharedHandle")
            };
            let open_fence = |h: isize| -> Result<ID3D12Fence> {
                let mut r = None;
                unsafe { raw.OpenSharedHandle(HANDLE(h as *mut _), &mut r) }.context("geteilte Fence öffnen")?;
                r.context("OpenSharedHandle")
            };
            let res = ring.textures.iter().map(|&h| open_res(h)).collect::<Result<Vec<_>>>()?;
            (res, open_fence(ring.ready_fence)?, open_fence(ring.released_fence)?)
        };
        let size = wgpu::Extent3d { width: ring.width, height: ring.height, depth_or_array_layers: 1 };
        let slots = resources
            .into_iter()
            .map(|res| {
                // SAFETY: NV12-Ressource passender Größe, nur zum Lesen im Shader (Zustand COMMON → implizit lesbar).
                let tex = unsafe {
                    let hal_tex = wgpu::hal::dx12::Device::texture_from_raw(res, wgpu::TextureFormat::NV12, wgpu::TextureDimension::D2, size, 1, 1);
                    self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                        hal_tex,
                        &wgpu::TextureDescriptor {
                            label: Some("veyra-shared"),
                            size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::NV12,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING,
                            view_formats: &[],
                        },
                        wgpu::wgt::TextureUses::RESOURCE,
                    )
                };
                let plane = |format, aspect| tex.create_view(&wgpu::TextureViewDescriptor { format: Some(format), aspect, ..Default::default() });
                (
                    plane(wgpu::TextureFormat::R8Unorm, wgpu::TextureAspect::Plane0),
                    plane(wgpu::TextureFormat::Rg8Unorm, wgpu::TextureAspect::Plane1),
                    GENERATION.fetch_add(1, Ordering::Relaxed),
                )
            })
            .collect();
        log::info!("Zero-Copy aktiv: {} geteilte Texturen {}x{}", ring.textures.len(), ring.width, ring.height);
        Ok(VideoTex {
            planes: Planes::Shared(SharedTex { ring: ring.id, slots, current: None, ready, released, render_value: Cell::new(0) }),
            width: ring.width,
            height: ring.height,
            crop: [0.0, 0.0, 1.0, 1.0],
            bt709: true,
        })
    }

}

impl Gpu {
    /// Widget-Bild (RGBA, vormultipliziert) für `target` hochladen; die Texturen werden wiederverwendet,
    /// solange die Größe gleich bleibt. `glow` ist leer oder gleich groß wie `rgba`.
    pub fn overlay(&self, current: Option<Overlay>, target: &Target, size: (u32, u32), rgba: &[u8], glow: &[u8], at: (i32, i32)) -> Overlay {
        let d = self.dev();
        let ((width, height), (x, y)) = (size, at);
        let mut ov = match current {
            Some(o) if o.size == (width, height) => o,
            _ => {
                let tex = |label| {
                    d.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    })
                };
                let texture = tex("veyra-widget");
                let glow_tex = tex("veyra-widget-glow");
                let uniforms = d.device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 32,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let view = texture.create_view(&Default::default());
                let glow_view = glow_tex.create_view(&Default::default());
                let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &d.overlay_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&glow_view) },
                    ],
                });
                // Neue Texturen sind leer (wgpu initialisiert sie mit 0) – also auch kein Leuchten.
                Overlay { texture, glow: glow_tex, has_glow: false, uniforms, bind, size: (width, height), flash: Cell::new(0.0) }
            }
        };
        let upload = |texture: &wgpu::Texture, data: &[u8]| {
            d.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: None },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
        };
        upload(&ov.texture, rgba);
        if !glow.is_empty() {
            upload(&ov.glow, glow);
            ov.has_glow = true;
        } else if ov.has_glow {
            upload(&ov.glow, &vec![0u8; rgba.len()]);
            ov.has_glow = false;
        }
        let u: [f32; 8] = [x as f32, y as f32, width as f32, height as f32, target.config.width as f32, target.config.height as f32, ov.flash.get(), 0.0];
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        d.queue.write_buffer(&ov.uniforms, 0, &bytes);
        ov
    }

    /// Einen Ausschnitt der Leucht-Ebene ersetzen (Leuchtspur des Signal Analyzers, pro Bild).
    /// `rect` = x, y, Breite, Höhe innerhalb des Widget-Bildes; `rgba` hat genau diese Größe.
    pub fn overlay_glow(&self, ov: &mut Overlay, rect: (u32, u32, u32, u32), rgba: &[u8]) {
        let (x, y, w, h) = rect;
        if w == 0 || h == 0 || x + w > ov.size.0 || y + h > ov.size.1 || rgba.len() != (w * h * 4) as usize {
            return;
        }
        self.dev().queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &ov.glow, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        ov.has_glow = true;
    }

    /// Stärke des Leuchtens setzen (pro Bild, für den Countdown-Effekt). Schreibt nur bei Änderung.
    pub fn set_flash(&self, ov: &Overlay, flash: f32) {
        if (ov.flash.get() - flash).abs() > 1e-3 {
            ov.flash.set(flash);
            self.dev().queue.write_buffer(&ov.uniforms, 24, &flash.to_le_bytes());
        }
    }

    pub fn render<'a>(&self, target: &mut Target, tex: &VideoTex, p: &DrawParams, overlays: impl IntoIterator<Item = &'a Overlay>) {
        let d = self.dev();
        let Some((y_view, uv_view, generation)) = tex.views() else { return };
        let bind = match target.binds.iter().position(|(g, _)| *g == generation) {
            Some(i) => i,
            None => {
                let bg = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &d.layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: target.uniforms.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(y_view) },
                        wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(uv_view) },
                        wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&d.sampler) },
                    ],
                });
                // Klein halten: ein Ring hat wenige Slots, ältere Einträge gehören zu verworfenen Texturen.
                if target.binds.len() >= crate::video::RING + 2 {
                    target.binds.remove(0);
                }
                target.binds.push((generation, bg));
                target.binds.len() - 1
            }
        };

        let vis_w = tex.width as f32 * tex.crop[2];
        let vis_h = tex.height as f32 * tex.crop[3];
        let fit = match p.fit {
            Fit::Cover => 0.0,
            Fit::Contain => 1.0,
            Fit::Stretch => 2.0,
        };
        let effect = match p.effect {
            Effect::None => 0.0,
            Effect::Pulse => 1.0,
            Effect::Glow => 2.0,
            Effect::Chroma => 3.0,
        };
        let l = p.levels;
        let u: [f32; 20] = [
            target.config.width as f32, target.config.height as f32, vis_w, vis_h,
            tex.crop[0], tex.crop[1], tex.crop[2], tex.crop[3],
            l.bass, l.mid, l.high, l.level,
            fit, effect, p.intensity, p.time,
            if tex.bt709 { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0,
        ];
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        d.queue.write_buffer(&target.uniforms, 0, &bytes);

        let frame = match target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                target.surface.configure(&d.device, &target.config);
                return;
            }
            other => {
                log::debug!("Bild übersprungen: {other:?}");
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = d.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&d.pipeline);
            pass.set_bind_group(0, &target.binds[bind].1, &[]);
            pass.draw(0..3, 0..1);
            let mut overlays = overlays.into_iter().peekable();
            if overlays.peek().is_some() {
                pass.set_pipeline(&d.overlay_pipeline);
                for o in overlays {
                    pass.set_bind_group(0, &o.bind, &[]);
                    pass.draw(0..4, 0..1);
                }
            }
        }
        // Zero-Copy: nach diesem Zeichnen darf der Dekoder den Slot wieder überschreiben.
        #[cfg(windows)]
        if let Planes::Shared(s) = &tex.planes {
            if let Some(q) = unsafe { d.queue.as_hal::<wgpu::hal::api::Dx12>() } {
                let v = s.render_value.get() + 1;
                q.add_signal_fence(s.released.clone(), v);
                s.render_value.set(v);
            }
        }
        d.queue.submit([enc.finish()]);
        d.queue.present(frame);
    }
}
