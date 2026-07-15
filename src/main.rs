//! Window + display path. The game renders on the CPU into a 320x200
//! `Framebuffer`; the GPU's only job is to upload that buffer as a texture
//! each frame and stretch it onto the window (nearest-neighbor, letterboxed
//! to 4:3 — the original's display aspect).

use wolf3d::{demo, fb, game};

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use fb::{Framebuffer, HEIGHT, WIDTH};
use game::{Game, Input};

// =============================================================================
// GPU BLITTER
// =============================================================================

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
}

const SHADER: &str = r#"
struct VSOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VSOut {
    // Fullscreen triangle covering clip space; uv y flipped (texture row 0 = top).
    var p = array<vec2f, 3>(vec2f(-1.0, -3.0), vec2f(3.0, 1.0), vec2f(-1.0, 1.0));
    var out: VSOut;
    out.pos = vec4f(p[i], 0.0, 1.0);
    out.uv = vec2f((p[i].x + 1.0) * 0.5, (1.0 - p[i].y) * 0.5);
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs(in: VSOut) -> @location(0) vec4f {
    return textureSample(tex, samp, in.uv);
}
"#;

impl Gpu {
    fn new(window: Arc<Window>) -> Self {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance.create_surface(window.clone()).expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .expect("no adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .expect("no device");

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .expect("surface unsupported");
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("framebuffer"),
            size: wgpu::Extent3d {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(config.format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        Self { surface, device, queue, config, pipeline, bind_group, texture }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, fb: &Framebuffer) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            fb.as_bytes(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((WIDTH * 4) as u32),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: WIDTH as u32,
                height: HEIGHT as u32,
                depth_or_array_layers: 1,
            },
        );

        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(f) | Cst::Suboptimal(f) => f,
            Cst::Outdated | Cst::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Cst::Timeout | Cst::Occluded | Cst::Validation => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("blit"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            });

            // Letterbox to 4:3 (the game is 320x200 shown on a 4:3 monitor).
            let (ww, wh) = (self.config.width as f32, self.config.height as f32);
            let target = 4.0 / 3.0;
            let (vw, vh) = if ww / wh > target { (wh * target, wh) } else { (ww, ww / target) };
            pass.set_viewport((ww - vw) / 2.0, (wh - vh) / 2.0, vw, vh, 0.0, 1.0);

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}

// =============================================================================
// APP / GAME LOOP
// =============================================================================

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    fb: Framebuffer,
    game: Game,
    keys: HashSet<KeyCode>,
    use_pressed: bool,
    weapon_pressed: Option<u8>,
    last_frame: Instant,
    fps_frames: u32,
    fps_since: Instant,
    fps: u32,
}

impl App {
    fn new(game: Game) -> Self {
        Self {
            window: None,
            gpu: None,
            fb: Framebuffer::new(),
            game,
            keys: HashSet::new(),
            use_pressed: false,
            weapon_pressed: None,
            last_frame: Instant::now(),
            fps_frames: 0,
            fps_since: Instant::now(),
            fps: 0,
        }
    }

    fn refresh_title(&self) {
        if let Some(w) = &self.window {
            w.set_title(&format!(
                "wolf3d — {} ({}/{}) — {} fps",
                self.game.world.level.name,
                self.game.level_idx + 1,
                self.game.maps.num_levels(),
                self.fps,
            ));
        }
    }

    fn update(&mut self, dt: f32) {
        let down = |k: KeyCode| self.keys.contains(&k);
        let input = Input {
            forward: down(KeyCode::KeyW) || down(KeyCode::ArrowUp),
            back: down(KeyCode::KeyS) || down(KeyCode::ArrowDown),
            strafe_left: down(KeyCode::KeyA),
            strafe_right: down(KeyCode::KeyD),
            turn_left: down(KeyCode::ArrowLeft),
            turn_right: down(KeyCode::ArrowRight),
            run: down(KeyCode::ShiftLeft),
            use_door: std::mem::take(&mut self.use_pressed),
            select_weapon: self.weapon_pressed.take(),
        };
        self.game.update(dt, &input);
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("wolf3d")
                        .with_inner_size(winit::dpi::LogicalSize::new(960.0, 720.0)),
                )
                .expect("create window"),
        );
        self.gpu = Some(Gpu::new(window.clone()));
        self.window = Some(window);
        self.last_frame = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if event.state.is_pressed() && !event.repeat {
                        match code {
                            KeyCode::KeyQ => event_loop.exit(),
                            KeyCode::KeyF | KeyCode::F11 => {
                                if let Some(w) = &self.window {
                                    set_fullscreen(w, !is_fullscreen(w));
                                }
                            }
                            // Esc leaves fullscreen; quits when already windowed.
                            KeyCode::Escape => {
                                if let Some(w) = &self.window {
                                    if is_fullscreen(w) {
                                        set_fullscreen(w, false);
                                    } else {
                                        event_loop.exit();
                                    }
                                }
                            }
                            KeyCode::Space | KeyCode::KeyE => self.use_pressed = true,
                            KeyCode::Digit1 => self.weapon_pressed = Some(0),
                            KeyCode::Digit2 => self.weapon_pressed = Some(1),
                            KeyCode::Digit3 => self.weapon_pressed = Some(2),
                            KeyCode::Digit4 => self.weapon_pressed = Some(3),
                            KeyCode::KeyN => {
                                self.game.switch_level(1);
                                self.refresh_title();
                            }
                            KeyCode::KeyP => {
                                self.game.switch_level(-1);
                                self.refresh_title();
                            }
                            _ => {}
                        }
                    }
                    if event.state.is_pressed() {
                        self.keys.insert(code);
                    } else {
                        self.keys.remove(&code);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;

                self.update(dt);
                self.game.render(&mut self.fb);
                if let Some(gpu) = &mut self.gpu {
                    gpu.render(&self.fb);
                }

                self.fps_frames += 1;
                if self.fps_since.elapsed().as_secs_f32() >= 1.0 {
                    self.fps = self.fps_frames;
                    self.refresh_title();
                    self.fps_frames = 0;
                    self.fps_since = now;
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

// macOS native (Spaces) fullscreen silently no-ops for non-bundled binaries
// like a bare cargo build, so use winit's "simple fullscreen" there instead.
#[cfg(target_os = "macos")]
fn is_fullscreen(w: &Window) -> bool {
    use winit::platform::macos::WindowExtMacOS;
    w.simple_fullscreen()
}

#[cfg(target_os = "macos")]
fn set_fullscreen(w: &Window, on: bool) {
    use winit::platform::macos::WindowExtMacOS;
    w.set_simple_fullscreen(on);
}

#[cfg(not(target_os = "macos"))]
fn is_fullscreen(w: &Window) -> bool {
    w.fullscreen().is_some()
}

#[cfg(not(target_os = "macos"))]
fn set_fullscreen(w: &Window, on: bool) {
    w.set_fullscreen(on.then_some(winit::window::Fullscreen::Borderless(None)));
}

fn main() {
    // WOLF3D_LEVEL=n starts on level n (1-based), handy for debugging.
    let level_idx = std::env::var("WOLF3D_LEVEL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(0, |n| n.saturating_sub(1));
    let mut game = Game::new(level_idx);

    // WOLF3D_DEMO="w:1;use;wait:1;snap:door" plays scripted input headless
    // (no window) and writes framebuffer snapshots; see src/demo.rs.
    if let Ok(script) = std::env::var("WOLF3D_DEMO") {
        demo::run(&mut game, &script);
        return;
    }

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut App::new(game)).expect("run");
}
