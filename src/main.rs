//! Window + display path. The game renders on the CPU into a 320x200
//! `Framebuffer`; the GPU's only job is to upload that buffer as a texture
//! each frame and stretch it onto the window (nearest-neighbor, letterboxed
//! to 4:3 — the original's display aspect).

use wolf3d::{config, demo, fb, game};

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use wolf3d::demorec::Demo;

use wolf3d::assets::audio::AudioData;
use wolf3d::sound::{Backend, SoundAssets};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use fb::{Framebuffer, HEIGHT, WIDTH};
use game::{Game, GameScreen, Input, SfxMode};

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
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
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

        Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            texture,
        }
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
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
            let (vw, vh) = if ww / wh > target {
                (wh * target, wh)
            } else {
                (ww, ww / target)
            };
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

/// Radians of turn per raw mouse count. Tuned for a Quake-ish feel at normal
/// macOS mouse speed.
const MOUSE_SENSITIVITY: f32 = 0.0022;

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    fb: Framebuffer,
    game: Game,
    /// Audio output (None when no device / data is available — the game runs
    /// silent). The simulation never touches this; it consumes drained events.
    sound: Option<Backend>,
    /// The music track currently requested, so we only re-issue on change.
    current_music: Option<usize>,
    /// Sound-menu state last pushed to the backend (re-issue only on change).
    applied_sfx_mode: SfxMode,
    applied_music_on: bool,
    /// The persistent options last written to disk (re-write only on change).
    applied_config: config::Config,
    /// `WOLF3D_RECORD=path`: write the next play session (level start until
    /// death / level exit / Esc) as an attract demo to this path. Taken (set to
    /// None) once the recording has been written.
    record_path: Option<PathBuf>,
    /// The recording being captured, once a play session has started.
    recording: Option<Demo>,
    /// Fixed-timestep accumulator used while a recording is armed (recordings
    /// must advance in whole 70 Hz tics to be replayable).
    tic_accum: f32,
    keys: HashSet<KeyCode>,
    use_pressed: bool,
    weapon_pressed: Option<u8>,
    mouse_fire: bool,
    /// A character typed this frame (for the save-name field).
    typed: Option<char>,
    /// Backspace pressed this frame.
    backspace: bool,
    /// Accumulated raw mouse x-delta since the last tic (counts, unscaled).
    mouse_dx: f32,
    /// Whether the cursor is currently grabbed for mouse-look.
    cursor_grabbed: bool,
    // Edge-triggered menu navigation (consumed once per key-down).
    menu_up: bool,
    menu_down: bool,
    menu_left: bool,
    menu_right: bool,
    menu_enter: bool,
    menu_back: bool,
    any_key: bool,
    last_frame: Instant,
    fps_frames: u32,
    fps_since: Instant,
    fps: u32,
}

impl App {
    fn new(game: Game, sound: Option<Backend>, record_path: Option<PathBuf>) -> Self {
        let (sfx_mode, music_on) = (game.sfx_mode, game.music_on);
        let applied_config = game.config_snapshot();
        Self {
            window: None,
            gpu: None,
            fb: Framebuffer::new(),
            game,
            sound,
            current_music: None,
            applied_sfx_mode: sfx_mode,
            applied_music_on: music_on,
            applied_config,
            record_path,
            recording: None,
            tic_accum: 0.0,
            keys: HashSet::new(),
            use_pressed: false,
            weapon_pressed: None,
            mouse_fire: false,
            typed: None,
            backspace: false,
            mouse_dx: 0.0,
            cursor_grabbed: false,
            menu_up: false,
            menu_left: false,
            menu_right: false,
            menu_down: false,
            menu_enter: false,
            menu_back: false,
            any_key: false,
            last_frame: Instant::now(),
            fps_frames: 0,
            fps_since: Instant::now(),
            fps: 0,
        }
    }

    /// Grab and hide the cursor during play (mouse-look), release it in menus.
    fn sync_cursor_grab(&mut self) {
        let want = self.game.screen == GameScreen::Playing;
        if want == self.cursor_grabbed {
            return;
        }
        if let Some(w) = &self.window {
            use winit::window::CursorGrabMode;
            let mode = if want {
                CursorGrabMode::Locked
            } else {
                CursorGrabMode::None
            };
            // Locked is supported on macOS; fall back to Confined elsewhere.
            let ok = w
                .set_cursor_grab(mode)
                .or_else(|_| {
                    w.set_cursor_grab(if want {
                        CursorGrabMode::Confined
                    } else {
                        CursorGrabMode::None
                    })
                })
                .is_ok();
            if ok {
                w.set_cursor_visible(!want);
                self.cursor_grabbed = want;
            }
        }
    }

    fn refresh_title(&self) {
        if let Some(w) = &self.window {
            let mut cheats = String::new();
            if self.game.god {
                cheats.push_str(" [GOD]");
            }
            if self.game.infinite_ammo {
                cheats.push_str(" [AMMO]");
            }
            w.set_title(&format!(
                "wolf3d — {} ({}/{}) — {} fps{cheats}",
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
            run: down(KeyCode::ShiftLeft) || down(KeyCode::ShiftRight),
            use_door: std::mem::take(&mut self.use_pressed),
            select_weapon: self.weapon_pressed.take(),
            fire: down(KeyCode::Space)
                || down(KeyCode::ControlLeft)
                || down(KeyCode::ControlRight)
                || self.mouse_fire,
            turn_delta: std::mem::take(&mut self.mouse_dx)
                * MOUSE_SENSITIVITY
                * self.game.mouse_sensitivity_scale(),
            menu_up: std::mem::take(&mut self.menu_up),
            menu_down: std::mem::take(&mut self.menu_down),
            menu_left: std::mem::take(&mut self.menu_left),
            menu_right: std::mem::take(&mut self.menu_right),
            menu_enter: std::mem::take(&mut self.menu_enter),
            menu_back: std::mem::take(&mut self.menu_back),
            any_key: std::mem::take(&mut self.any_key),
            typed: std::mem::take(&mut self.typed),
            backspace: std::mem::take(&mut self.backspace),
        };
        if self.record_path.is_some() {
            self.update_recording(dt, input);
        } else {
            self.game.update(dt, &input);
        }
        self.sync_audio();
        self.persist_config();
    }

    /// Fixed-timestep update used while `WOLF3D_RECORD` is armed: the game
    /// advances in whole 70 Hz tics, the play session's inputs are captured one
    /// per tic, and a visibility-marking render runs after every play tic so the
    /// recording's render side effects are embedded at tic granularity (see
    /// `demorec`'s determinism notes). The demo is written when the session ends
    /// (death / level exit / Esc back to the menu).
    fn update_recording(&mut self, dt: f32, mut input: Input) {
        // Cap the backlog so a stall can't spiral into a huge catch-up burst.
        self.tic_accum = (self.tic_accum + dt).min(0.25);
        while self.tic_accum >= game::TIC {
            self.tic_accum -= game::TIC;
            if let Some(d) = &mut self.recording {
                d.push(&input);
            }
            self.game.update(game::TIC, &input);

            if self.game.screen == GameScreen::Playing {
                if self.recording.is_none() {
                    // The play session just started: capture the header at the
                    // fresh level state, before any recorded tic.
                    let mut d = Demo::begin(&self.game);
                    d.windowed = true;
                    self.recording = Some(d);
                    println!("WOLF3D_RECORD: recording started");
                }
                // Embed this tic's render-visibility marking (playback re-runs
                // the same marking render after every tic).
                self.game.render(&mut self.fb);
            } else if let Some(d) = self.recording.take() {
                // The session ended (death / level exit / Esc): write the demo.
                let path = self.record_path.take().expect("record path present");
                match d.write_to(&path) {
                    Ok(()) => {
                        println!(
                            "WOLF3D_RECORD: wrote {} ({} tics)",
                            path.display(),
                            d.tics.len()
                        )
                    }
                    Err(e) => eprintln!("WOLF3D_RECORD: failed to write demo: {e}"),
                }
                return;
            }

            // Edge-triggered fields fire on the first tic of the frame only;
            // held keys persist across the frame's remaining tics.
            input = Input {
                use_door: false,
                select_weapon: None,
                turn_delta: 0.0,
                menu_up: false,
                menu_down: false,
                menu_left: false,
                menu_right: false,
                menu_enter: false,
                menu_back: false,
                any_key: false,
                typed: None,
                backspace: false,
                ..input
            };
        }
    }

    /// Write the persistent options (view size, sound, music, mouse sensitivity)
    /// to disk whenever they change (WL_MENU.C wrote CONFIG.WL6 on each edit).
    fn persist_config(&mut self) {
        let snap = self.game.config_snapshot();
        if snap != self.applied_config {
            self.applied_config = snap;
            if let Err(e) = config::save(&snap) {
                eprintln!("failed to save config: {e}");
            }
        }
    }

    /// Feed the tic's emitted sound events to the backend and keep the music and
    /// the Sound-menu options (effects mode, music on/off) in step with the game.
    fn sync_audio(&mut self) {
        let sounds = self.game.take_sounds();
        let screen = self.game.screen;
        let level = self.game.level_idx;
        let sfx_mode = self.game.sfx_mode;
        let music_on = self.game.music_on;
        let Some(backend) = &mut self.sound else {
            return;
        };

        // Apply Sound-menu changes to the backend only when they flip.
        if sfx_mode != self.applied_sfx_mode {
            self.applied_sfx_mode = sfx_mode;
            backend.set_digi_enabled(sfx_mode == SfxMode::DigiAdlib);
        }
        if music_on != self.applied_music_on {
            self.applied_music_on = music_on;
            backend.set_music_enabled(music_on);
        }

        // Effects are gated by the Sound menu's effects mode.
        if sfx_mode != SfxMode::Off {
            for id in sounds {
                backend.play(id);
            }
        }

        // Title is silent; the menus share the menu song; play the level song
        // while playing.
        let desired = match screen {
            GameScreen::Title | GameScreen::Credits => None,
            GameScreen::Playing
            | GameScreen::Attract
            | GameScreen::GetPsyched
            | GameScreen::Death
            | GameScreen::DeathCam => Some(self.game.variant.song_for_level(level)),
            GameScreen::Intermission => Some(self.game.variant.endlevel_song()),
            GameScreen::Victory
            | GameScreen::EndText
            | GameScreen::HighScores
            | GameScreen::HighScoreEntry => Some(self.game.variant.victory_song()),
            _ => Some(self.game.variant.menu_song()),
        };
        if desired != self.current_music {
            self.current_music = desired;
            backend.set_music(desired);
        }
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
            WindowEvent::MouseInput { state, button, .. } => match button {
                winit::event::MouseButton::Left => self.mouse_fire = state.is_pressed(),
                winit::event::MouseButton::Right if state.is_pressed() => {
                    self.use_pressed = true;
                }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let up = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y > 0.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
                };
                let next = if up {
                    (self.game.weapon + 1) % 4
                } else {
                    (self.game.weapon + 3) % 4
                };
                self.weapon_pressed = Some(next as u8);
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if event.state.is_pressed() && !event.repeat && self.game.is_text_entry() {
                        // Typing a save name: route printable text and editing
                        // keys to the field; suppress gameplay/global shortcuts
                        // so keys like Q/M don't quit or toggle music mid-name.
                        match code {
                            KeyCode::Escape => self.menu_back = true,
                            KeyCode::Enter | KeyCode::NumpadEnter => self.menu_enter = true,
                            KeyCode::Backspace => self.backspace = true,
                            _ => {
                                if let Some(text) = &event.text
                                    && let Some(c) = text.chars().next().filter(|c| !c.is_control())
                                {
                                    self.typed = Some(c);
                                }
                            }
                        }
                    } else if event.state.is_pressed() && !event.repeat && self.game.is_confirm() {
                        // CP_Quit's wait loop takes Y / N / Esc and nothing
                        // else. Y reads as accept and N/Esc as cancel, so the
                        // game side needs no separate yes/no inputs.
                        match code {
                            KeyCode::KeyY => self.menu_enter = true,
                            KeyCode::KeyN | KeyCode::Escape => self.menu_back = true,
                            _ => {}
                        }
                    } else if event.state.is_pressed() && !event.repeat {
                        let playing = self.game.screen == GameScreen::Playing;
                        // Any key-down advances the title screen.
                        self.any_key = true;
                        match code {
                            KeyCode::KeyQ => event_loop.exit(),
                            KeyCode::KeyF | KeyCode::F11 => {
                                if let Some(w) = &self.window {
                                    set_fullscreen(w, !is_fullscreen(w));
                                }
                            }
                            // Esc leaves fullscreen; otherwise it drives the
                            // menu (pause from play / back out of a menu).
                            KeyCode::Escape => {
                                let fs = self.window.as_ref().is_some_and(|w| is_fullscreen(w));
                                if fs {
                                    if let Some(w) = &self.window {
                                        set_fullscreen(w, false);
                                    }
                                } else {
                                    self.menu_back = true;
                                }
                            }
                            KeyCode::ArrowUp | KeyCode::KeyW => self.menu_up = true,
                            KeyCode::ArrowDown | KeyCode::KeyS => self.menu_down = true,
                            KeyCode::ArrowLeft | KeyCode::KeyA => self.menu_left = true,
                            KeyCode::ArrowRight | KeyCode::KeyD => self.menu_right = true,
                            KeyCode::Enter | KeyCode::NumpadEnter => self.menu_enter = true,
                            KeyCode::KeyM => {
                                // Toggle music through the game state so the
                                // Sound menu and the shortcut stay in sync.
                                self.game.music_on = !self.game.music_on;
                                println!(
                                    "music: {}",
                                    if self.game.music_on { "on" } else { "off" }
                                );
                            }
                            KeyCode::KeyE => self.use_pressed = true,
                            KeyCode::Digit1 => self.weapon_pressed = Some(0),
                            KeyCode::Digit2 => self.weapon_pressed = Some(1),
                            KeyCode::Digit3 => self.weapon_pressed = Some(2),
                            KeyCode::Digit4 => self.weapon_pressed = Some(3),
                            // --- Cheats (during play only) ---
                            KeyCode::Digit6 if playing => {
                                self.game.level_sel = self.game.level_idx;
                                self.game.screen = GameScreen::LevelSelect;
                            }
                            KeyCode::Digit7 if playing => {
                                self.game.god = !self.game.god;
                                self.refresh_title();
                            }
                            // Free items: full ammo and both keys (all four
                            // weapons are always selectable in this port).
                            KeyCode::Digit8 if playing => {
                                self.game.ammo = 99;
                                self.game.keys = wolf3d::hud::KEY_GOLD | wolf3d::hud::KEY_SILVER;
                            }
                            KeyCode::Digit9 if playing => {
                                self.game.infinite_ammo = !self.game.infinite_ammo;
                                self.refresh_title();
                            }
                            // The classic MLI cheat: full health/ammo/keys,
                            // score wiped to zero.
                            KeyCode::Digit0 if playing => {
                                self.game.health = 100;
                                self.game.ammo = 99;
                                self.game.keys = wolf3d::hud::KEY_GOLD | wolf3d::hud::KEY_SILVER;
                                self.game.score = 0;
                            }
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
                if self.game.should_quit {
                    event_loop.exit();
                    return;
                }
                self.sync_cursor_grab();
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

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: winit::event::DeviceEvent,
    ) {
        // Raw (unaccelerated) motion for mouse-look; only while grabbed so the
        // menu cursor never turns the player.
        if let winit::event::DeviceEvent::MouseMotion { delta } = event
            && self.cursor_grabbed
        {
            self.mouse_dx += delta.0 as f32;
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
    let level_env = std::env::var("WOLF3D_LEVEL").ok();
    let level_idx = level_env
        .as_ref()
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(0, |n| n.saturating_sub(1));
    let mut game = Game::new(level_idx);

    // Boot to the title/menu unless a level is pinned. A gameplay demo script
    // (no `key:` commands) keeps booting straight into play as before; the
    // windowed app and any menu demo (which drives the menu with `key:`) boot
    // to the title screen.
    let demo_script = std::env::var("WOLF3D_DEMO").ok();
    let starts_at_menu =
        level_env.is_none() && demo_script.as_deref().is_none_or(|s| s.contains("key:"));
    // The "Get Psyched!" load screen only runs in the plain windowed game; the
    // demo and WOLF3D_LEVEL paths skip it (instant loads, deterministic runs).
    game.show_load_screen = level_env.is_none() && demo_script.is_none();
    if starts_at_menu {
        game.to_title();
    }

    // The attract loop's recorded demos (demos/*.dm); missing demos just skip
    // the demo stage of the title cycle. Loaded for the headless path too, so
    // scripts can exercise the attract loop; game.demos has no effect on the
    // simulation until a demo is actually played.
    game.load_attract_demos();

    // WOLF3D_DEMO="w:1;use;wait:1;snap:door" plays scripted input headless
    // (no window) and writes framebuffer snapshots; see src/demo.rs.
    if let Some(script) = demo_script {
        demo::run(&mut game, &script);
        return;
    }

    // Load persistent options (view size, sound, music, mouse sensitivity) for
    // the windowed game only. The demo/tests never read a config, so their
    // render path stays at the full-size default and remains pixel-identical.
    if let Some(cfg) = config::load() {
        game.apply_config(&cfg);
    }

    // WOLF3D_RECORD=path: capture the next play session (level start until
    // death / level exit / Esc) as an attract demo. Recorded sessions run at a
    // fixed 70 Hz timestep and embed render-visibility effects per tic.
    let record_path = std::env::var("WOLF3D_RECORD").ok().map(PathBuf::from);

    // Open the audio device for the windowed path. Any failure (no device, no
    // sound data) is non-fatal: the game just runs silent.
    let sound = match AudioData::load_ext(&wolf3d::assets::data_dir(), game.variant.ext) {
        Ok(audio) => {
            let assets = SoundAssets::new(audio, game.vswap.digi.clone());
            match Backend::start(assets) {
                Ok(b) => Some(b),
                Err(e) => {
                    eprintln!("audio disabled: {e}");
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("audio disabled: could not load AUDIOT/AUDIOHED: {e}");
            None
        }
    };

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop
        .run_app(&mut App::new(game, sound, record_path))
        .expect("run");
}
