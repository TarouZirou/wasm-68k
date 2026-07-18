//! wgpu ベースのレンダラ。ネイティブ (Vulkan 等) と Wasm (WebGPU / WebGL2) で共用する。
//!
//! X68000 のフレームバッファ (16bit GRBi) を `R16Uint` テクスチャとしてアップロードし、
//! WGSL シェーダで RGB 変換とアスペクト比維持スケーリングを行う。
//! WebGPU が使えない環境では wgpu の WebGL2 バックエンドに自動フォールバックする。

use anyhow::{Context, Result, anyhow};

/// wgpu で使用中の描画バックエンド。
pub use wgpu::Backend as RenderBackend;

/// フレームバッファテクスチャの最大サイズ (X68000 の最大画面 1024x1024)。
const MAX_FRAME_WIDTH: u32 = 1024;
const MAX_FRAME_HEIGHT: u32 = 1024;

/// フレームバッファを画面に表示するためのレンダラ。
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    frame_texture: wgpu::Texture,
    crt_buffer: wgpu::Buffer,
    frame_size: (u32, u32),
    backend: RenderBackend,
}

impl Renderer {
    /// レンダラを初期化する。
    ///
    /// `target` はネイティブでは `Arc<winit::window::Window>`を渡す。
    /// Wasm のcanvasは [`Renderer::new_for_canvas`] で初期化する。
    /// `width` / `height` はサーフェス (ウィンドウ / canvas) の初期サイズ。
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new_with_display(target, None, width, height).await
    }

    /// Web canvas向けの初期化。
    ///
    /// `SurfaceTarget::Canvas`はWasmビルドでのみ存在するため、このcrate内の
    /// target固有APIへ閉じ込める。これにより呼び出し側をホスト用rust-analyzerで
    /// 解析しても、Web専用variantを解決しようとしない。
    #[cfg(target_arch = "wasm32")]
    pub async fn new_for_canvas(
        canvas: web_sys::HtmlCanvasElement,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::new(wgpu::SurfaceTarget::Canvas(canvas), width, height).await
    }

    /// ディスプレイハンドル指定付きの初期化。
    ///
    /// ネイティブの GLES バックエンド (特に Wayland) でサーフェスを作成するには
    /// プラットフォームのディスプレイハンドルが必要なため、
    /// ネイティブランナーは winit の `OwnedDisplayHandle` をここに渡す。
    pub async fn new_with_display(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        display: Option<Box<dyn wgpu_types::WgpuHasDisplayHandle>>,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        // WebGPU が使えなければ WebGL2 にフォールバックする (Web ではそちらが有効)
        let descriptor = match display {
            Some(display) => wgpu::InstanceDescriptor::new_with_display_handle(display),
            None => wgpu::InstanceDescriptor::new_without_display_handle(),
        };
        let instance = wgpu::util::new_instance_with_webgpu_detection(descriptor).await;

        let surface = instance.create_surface(target).context("create_surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("request_adapter")?;

        let info = adapter.get_info();
        log::info!("wgpu adapter: {} (backend: {:?})", info.name, info.backend);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("x68k device"),
                // WebGL2 フォールバックでも動く最低限の limits
                required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                ..Default::default()
            })
            .await
            .context("request_device")?;

        let config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or_else(|| anyhow!("surface is not supported by the adapter"))?;
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("x68k frame shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("x68k frame bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("x68k pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("x68k frame pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let crt_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("x68k CRT options"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(
            &crt_buffer,
            0,
            bytemuck::cast_slice(&[0.0f32, 0.25, 0.12, 0.04]),
        );

        // 初期サイズのテクスチャを作り、実フレームサイズに合わせて随時作り直す
        let (frame_texture, bind_group) =
            Self::create_frame_resources(&device, &bind_group_layout, &crt_buffer, 1, 1);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group_layout,
            bind_group,
            frame_texture,
            crt_buffer,
            frame_size: (1, 1),
            backend: info.backend,
        })
    }

    /// 使用中の描画バックエンドを返す (WebGPU か WebGL2 フォールバックかの判別用)。
    pub fn backend(&self) -> RenderBackend {
        self.backend
    }

    /// CRT表示効果を更新する。値は0.0–1.0へ制限する。
    pub fn set_crt_options(
        &mut self,
        enabled: bool,
        scanline_strength: f32,
        mask_strength: f32,
        curvature: f32,
    ) {
        let values = [
            if enabled { 1.0 } else { 0.0 },
            scanline_strength.clamp(0.0, 1.0),
            mask_strength.clamp(0.0, 1.0),
            curvature.clamp(0.0, 0.25),
        ];
        self.queue
            .write_buffer(&self.crt_buffer, 0, bytemuck::cast_slice(&values));
    }

    /// サーフェス (ウィンドウ / canvas) のリサイズを通知する。
    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// 1 フレームを描画する。`frame` は `fw * fh` 個の GRBi ピクセル。
    pub fn render(&mut self, frame: &[u16], fw: u32, fh: u32) -> Result<()> {
        anyhow::ensure!(
            fw <= MAX_FRAME_WIDTH && fh <= MAX_FRAME_HEIGHT,
            "frame size {fw}x{fh} exceeds maximum"
        );
        anyhow::ensure!(
            frame.len() == (fw * fh) as usize,
            "frame buffer length mismatch"
        );

        // 画面モード変更で解像度が変わったらテクスチャを作り直す
        if self.frame_size != (fw, fh) {
            let (texture, bind_group) = Self::create_frame_resources(
                &self.device,
                &self.bind_group_layout,
                &self.crt_buffer,
                fw,
                fh,
            );
            self.frame_texture = texture;
            self.bind_group = bind_group;
            self.frame_size = (fw, fh);
        }

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.frame_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(frame),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(fw * 2),
                rows_per_image: Some(fh),
            },
            wgpu::Extent3d {
                width: fw,
                height: fh,
                depth_or_array_layers: 1,
            },
        );

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // このフレームはスキップして次に備える
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!("surface validation error"));
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("x68k frame encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("x68k frame pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // アスペクト比を維持してレターボックス配置する
            let (sw, sh) = (self.config.width as f32, self.config.height as f32);
            let scale = (sw / fw as f32).min(sh / fh as f32);
            let vw = fw as f32 * scale;
            let vh = fh as f32 * scale;
            pass.set_viewport((sw - vw) / 2.0, (sh - vh) / 2.0, vw, vh, 0.0, 1.0);

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit([encoder.finish()]);
        self.queue.present(surface_texture);
        Ok(())
    }

    /// フレームバッファ用テクスチャとバインドグループを作成する。
    fn create_frame_resources(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        crt_buffer: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("x68k frame texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("x68k frame bind group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: crt_buffer.as_entire_binding(),
                },
            ],
        });
        (texture, bind_group)
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vector {
        grbi: u16,
        rgb: [u8; 3],
    }

    fn shader_integer_conversion(pixel: u16) -> [u8; 3] {
        let pixel = u32::from(pixel);
        let intensity = pixel & 1;
        let g6 = ((pixel >> 10) & 0x3e) | intensity;
        let r6 = ((pixel >> 5) & 0x3e) | intensity;
        let b6 = (pixel & 0x3e) | intensity;
        [
            ((r6 << 2) | (r6 >> 4)) as u8,
            ((g6 << 2) | (g6 >> 4)) as u8,
            ((b6 << 2) | (b6 >> 4)) as u8,
        ]
    }

    #[test]
    fn wgsl_formula_matches_shared_grbi_vectors() {
        assert!(include_str!("shader.wgsl").contains("fn grbi_to_rgb8"));
        let vectors: Vec<Vector> = serde_json::from_str(include_str!(
            "../../x68k-core/tests/fixtures/grbi_vectors.json"
        ))
        .unwrap();
        for vector in vectors {
            assert_eq!(shader_integer_conversion(vector.grbi), vector.rgb);
        }
    }
}
