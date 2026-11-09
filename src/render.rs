//! GPU-Ausgabe mit wgpu: ein Gerät für alle Monitore, eine Oberfläche pro Monitor,
//! Videotexturen pro Datei (werden von allen Monitoren mit demselben Video geteilt).

use crate::audio::Levels;
use crate::config::{Backend, Effect, Fit, GpuPreference};
use crate::video::Frame;
use anyhow::{Context, Result};
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
}

/// Y- und UV-Textur eines Videos.
pub struct VideoTex {
    y: wgpu::Texture,
    uv: wgpu::Texture,
    y_view: wgpu::TextureView,
    uv_view: wgpu::TextureView,
    width: u32,
    height: u32,
    crop: [f32; 4],
    bt709: bool,
    generation: u64,
}

/// Eine Fensteroberfläche (ein Monitor).
pub struct Target {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    uniforms: wgpu::Buffer,
    bind: Option<(wgpu::BindGroup, u64)>,
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

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("veyra"),
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
        Ok(Self { adapter, device, queue, pipeline, layout, sampler, format })
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
        Ok(Target { surface, config, uniforms, bind: None })
    }

    /// Bild hochladen; legt Texturen neu an, wenn sich die Größe geändert hat.
    pub fn upload(&self, tex: &mut Option<VideoTex>, f: &Frame) {
        let d = self.dev();
        let fits = tex.as_ref().is_some_and(|t| t.width == f.width && t.height == f.height);
        if !fits {
            *tex = Some(d.create_video_tex(f.width, f.height));
        }
        let t = tex.as_mut().unwrap();
        t.crop = f.crop;
        t.bt709 = f.bt709;
        let uv_w = f.width.div_ceil(2);
        let uv_h = f.height.div_ceil(2);
        d.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t.y, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &f.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(f.pitch), rows_per_image: None },
            wgpu::Extent3d { width: f.width, height: f.height, depth_or_array_layers: 1 },
        );
        d.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t.uv, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &f.data,
            wgpu::TexelCopyBufferLayout { offset: f.uv_offset() as u64, bytes_per_row: Some(f.pitch), rows_per_image: None },
            wgpu::Extent3d { width: uv_w, height: uv_h, depth_or_array_layers: 1 },
        );
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
            y_view: y.create_view(&Default::default()),
            uv_view: uv.create_view(&Default::default()),
            y,
            uv,
            width,
            height,
            crop: [0.0, 0.0, 1.0, 1.0],
            bt709: true,
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
        }
    }

}

impl Gpu {
    pub fn render(&self, target: &mut Target, tex: &VideoTex, p: &DrawParams) {
        let d = self.dev();
        if target.bind.as_ref().map(|b| b.1) != Some(tex.generation) {
            let bg = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &d.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: target.uniforms.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex.y_view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tex.uv_view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&d.sampler) },
                ],
            });
            target.bind = Some((bg, tex.generation));
        }

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
            pass.set_bind_group(0, &target.bind.as_ref().unwrap().0, &[]);
            pass.draw(0..3, 0..1);
        }
        d.queue.submit([enc.finish()]);
        d.queue.present(frame);
    }
}
