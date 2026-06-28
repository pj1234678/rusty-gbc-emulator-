use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::gbc::cartridge::Cartridge;
use crate::gbc::joypad::{JoypadEvent, JoypadInput};
use crate::gbc::ppu::{FrameBuffer, GameboyRgb, LCD_HEIGHT, LCD_WIDTH};
use crate::gbc::Gameboy;

mod config;
mod gbc;

use sdl2::audio::AudioSpecDesired;
use sdl2::controller::{Button, GameController};
use sdl2::event::Event;
use sdl2::event::WindowEvent;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::render::{Canvas, Texture, TextureAccess};
use sdl2::video::Window;


struct FpsCounter {
    start_time: Instant,
    last_elapsed: Duration,
    frame_count: u64,
}

impl FpsCounter {
    /// The weight for older frames - current frame gets 1 - WEIGHT
    const WEIGHT: f32 = 0.1;

    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            last_elapsed: Duration::default(),
            frame_count: 0,
        }
    }

    /// Records a new frame and outputs the current FPS
    pub fn frame(&mut self) -> f32 {
        self.frame_count += 1;

        let elapsed = self.start_time.elapsed().as_millis() as f32;
        let last_elapsed = self.last_elapsed.as_millis() as f32;
        let weighted_duration = elapsed * (1.0 - Self::WEIGHT) + last_elapsed * Self::WEIGHT;
        let fps = self.frame_count as f32 / (weighted_duration / 1000.0);

        self.last_elapsed = self.start_time.elapsed();

        fps
    }

    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.frame_count = 0;
        self.last_elapsed = Duration::default();
    }
}



fn keycode_to_joypad_input(keycode: Keycode, config: &Config) -> Option<JoypadInput> {
    if keycode == config.key_a {
        Some(JoypadInput::A)
    } else if keycode == config.key_b {
        Some(JoypadInput::B)
    } else if keycode == config.key_start {
        Some(JoypadInput::Start)
    } else if keycode == config.key_select {
        Some(JoypadInput::Select)
    } else if keycode == config.key_up {
        Some(JoypadInput::Up)
    } else if keycode == config.key_down {
        Some(JoypadInput::Down)
    } else if keycode == config.key_left {
        Some(JoypadInput::Left)
    } else if keycode == config.key_right {
        Some(JoypadInput::Right)
    } else {
        None
    }
}

fn controller_button_to_joypad_input(button: Button, config: &Config) -> Option<JoypadInput> {
    if button == config.ctrl_a {
        Some(JoypadInput::A)
    } else if button == config.ctrl_b {
        Some(JoypadInput::B)
    } else if button == config.ctrl_start {
        Some(JoypadInput::Start)
    } else if button == config.ctrl_select {
        Some(JoypadInput::Select)
    } else if button == config.ctrl_up {
        Some(JoypadInput::Up)
    } else if button == config.ctrl_down {
        Some(JoypadInput::Down)
    } else if button == config.ctrl_left {
        Some(JoypadInput::Left)
    } else if button == config.ctrl_right {
        Some(JoypadInput::Right)
    } else {
        None
    }
}

/// Renders a single Gameboy frame via a raw pixel buffer and one SDL_UpdateTexture call.
///
/// Returns `Ok(())` on success, or an error string on failure (e.g. GPU context lost).
fn render_frame(
    frame_buffer: &FrameBuffer,
    canvas: &mut Canvas<Window>,
    texture: &mut Texture,
    outline: bool,
) -> Result<(), String> {
    let mut pixels = vec![0u8; LCD_WIDTH * LCD_HEIGHT * 4];

    for x in 0..LCD_WIDTH {
        for y in 0..LCD_HEIGHT {
            let GameboyRgb { red, green, blue } = frame_buffer.read(x, y);
            let offset = (y * LCD_WIDTH + x) * 4;
            pixels[offset] = blue;
            pixels[offset + 1] = green;
            pixels[offset + 2] = red;
            pixels[offset + 3] = 0xFF;
        }
    }

    if outline {
        for row in (0..LCD_HEIGHT).step_by(8) {
            for x in 0..LCD_WIDTH {
                let offset = (row * LCD_WIDTH + x) * 4;
                pixels[offset] = 0x80;
                pixels[offset + 1] = 0x80;
                pixels[offset + 2] = 0x80;
                pixels[offset + 3] = 0xFF;
            }
        }
        for col in (0..LCD_WIDTH).step_by(8) {
            for y in 0..LCD_HEIGHT {
                let offset = (y * LCD_WIDTH + col) * 4;
                pixels[offset] = 0x80;
                pixels[offset + 1] = 0x80;
                pixels[offset + 2] = 0x80;
                pixels[offset + 3] = 0xFF;
            }
        }
    }

    texture.update(None, &pixels, LCD_WIDTH * 4).map_err(|e| format!("texture.update: {}", e))?;
    canvas.copy(&texture, None, None).map_err(|e| format!("canvas.copy: {}", e))?;
    canvas.present();
    Ok(())
}

/// Handles a single Gameboy frame.
///
/// This advances the Gameboy for the number of CPU cycles in a single frame. Once the
/// underlying frame buffer is ready (i.e., on VBLANK), the frame is picked up and rendered
/// to an SDL texture.
///
/// At the end of the frame, any input joypad events are passed on to the Gameboy to be
/// picked up in the next frame.
fn handle_frame(
    gameboy: &mut Gameboy,
    canvas: &mut Canvas<Window>,
    texture: &mut Texture,
    joypad_events: &mut Vec<JoypadEvent>,
    outline: bool,
) -> Result<(), String> {
    // Run the Gameboy until the next frame is ready (i.e., start of VBLANK).
    //
    // This means we run from VBLANK to VBLANK. From the rendering side, it doesn't
    // really matter: as long as the frame is ready, we can render it! The emulator
    // will catch up & process the current VBLANK in the next call to this function.
    let frame_buffer = gameboy.frame(Some(joypad_events));

    // Clear out all processed input events
    joypad_events.clear();

    // Render the frame
    render_frame(frame_buffer, canvas, texture, outline)
}

fn new_persist_file(path: &PathBuf) -> std::fs::File {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

fn gui(rom_file: PathBuf, verbose: bool) {
    let scale = 4;
    let mut speed: u8 = 1;
    let boot_rom = false;
    let trace = false;
    let load = false;
    let rom_name = match rom_file.file_name() {
        None => None,
        Some(n) => Some(n.to_str().unwrap()),
    }
    .unwrap_or("Unknown ROM");

    if verbose {
        eprintln!("[DEBUG] ROM: {}", rom_name);
        eprintln!("[DEBUG] Verbose logging enabled");
    }

    let config = Config::load();

    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();
    let audio_subsystem = sdl_context.audio().unwrap();

    let controller_subsystem = sdl_context.game_controller().unwrap();
    let mut _controllers: Vec<GameController> = (0..controller_subsystem.num_joysticks().unwrap_or(0))
        .filter(|i| controller_subsystem.is_game_controller(*i))
        .filter_map(|i| controller_subsystem.open(i as u32).ok())
        .collect();
    let audio_device = audio_subsystem
        .open_queue::<i16, _>(None, &AudioSpecDesired {
            freq: Some(44100),
            channels: Some(2),
            samples: Some(256),
        })
        .unwrap();
    audio_device.pause();

    // Max queued audio before we drop samples to re-sync (in bytes).
    // ~3 frames worth: 3 * 739 stereo frames * 2 channels * 2 bytes = ~8868 bytes.
    let max_queued_bytes: usize = 8868;

    let width = LCD_WIDTH as u32 * scale;
    let height = LCD_HEIGHT as u32 * scale;

    // Setup an SDL2 Window
    let window = video_subsystem
        .window(rom_name, width, height)
        .position_centered()
        .allow_highdpi()
        .resizable()
        .build()
        .unwrap();

    // Convert the Window into a Canvas
    // This is what we will use to render content in the Window
    // TODO: Add flag for software vs. GPU
    let mut canvas = window.into_canvas().accelerated().build().unwrap();

    // Fix aspect ratio of canvas
    canvas.set_logical_size(width, height).unwrap();

    // Start in fullscreen
    let _ = canvas.window_mut().set_fullscreen(sdl2::video::FullscreenType::True);

    // Get a handle to the Canvas texture creator
    let texture_creator = canvas.texture_creator();

    // Create a Texture
    // We write raw pixel data here and copy it to the Canvas for rendering
    let mut texture = texture_creator
        .create_texture(
            Some(PixelFormatEnum::ARGB8888),
            TextureAccess::Streaming,
            LCD_WIDTH as u32,
            LCD_HEIGHT as u32,
        )
        .unwrap();

    let cartridge = get_cartridge(&rom_file, boot_rom);

    if verbose {
        let title = cartridge.title().unwrap_or("<unknown>");
        let cgb = cartridge.cgb();
        eprintln!("[DEBUG] Cartridge: \"{}\" (CGB: {})", title, cgb);
    }

    let save_state_path = &rom_file.with_extension("state");

    let mut gameboy = if load {
        // Load the Gameboy from an existing save state
        let data = std::fs::read(save_state_path).expect("Failed to open save state file");
        let gameboy =
            Gameboy::load(&data, cartridge).expect("Failed to load Gameboy from save state");
        gameboy
    } else {
        Gameboy::init(cartridge, trace).unwrap()
    };

    gameboy.set_verbose(verbose);

    let ram_path = &rom_file.with_extension("sav");
    let rtc_path = &rom_file.with_extension("rtc");
    let mut ram_persist = None;
    let mut rtc_persist = None;

    // Load persisted state, if any, into the `Gameboy`
    if gameboy.is_persist_required() {
        let ram_state = std::fs::read(ram_path).ok();
        let rtc_state = std::fs::read(rtc_path).ok();

        gameboy
            .unpersist(ram_state.as_ref(), rtc_state.as_ref())
            .expect("Failed to load persisted data");

        if gameboy.is_persist_ram() {
            ram_persist = Some(new_persist_file(ram_path));
        }

        if gameboy.is_persist_rtc() {
            rtc_persist = Some(new_persist_file(rtc_path));
        }
    }

    let mut paused = false;
    let mut outline = false;

    // List of joypad events to push to the Gameboy
    let mut joypad_events = Vec::new();


    let mut fps_counter = FpsCounter::new();

    let mut fast_forward = false;

    // Track whether the window is minimized or has lost focus (e.g. alt-tab).
    // When minimized we skip rendering to avoid SDL context-loss errors.
    let mut minimized = false;

    // Start the event loop
    let mut event_pump = sdl_context.event_pump().unwrap();
    'running: loop {
        let frame_time_ns = Gameboy::FRAME_DURATION / speed as u64;
        let frame_duration = Duration::from_nanos(frame_time_ns);

        let frame_start = Instant::now();

        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => break 'running,

                // Reset
                Event::KeyDown {
                    keycode: Some(Keycode::Semicolon),
                    ..
                } => {
                    if verbose {
                        eprintln!("[DEBUG] Reset triggered by user");
                    }
                    gameboy.reset();
                }

                // Emulation speed (no effect if in fast-forward mode)
                Event::KeyDown {
                    keycode: Some(Keycode::Equals),
                    ..
                } if !fast_forward => {
                    speed += 1;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Minus),
                    ..
                } if !fast_forward => {
                    speed -= 1;
                }
                Event::KeyDown {
                    keycode: Some(Keycode::Num0),
                    ..
                } if !fast_forward => {
                    speed = 1;
                }

                // Fast-forward mode (keyboard)
                Event::KeyDown {
                    keycode: Some(kc),
                    ..
                } if !fast_forward && kc == config.key_ff => {
                    fast_forward = true;
                    fps_counter.reset();
                }
                Event::KeyUp {
                    keycode: Some(kc),
                    ..
                } if fast_forward && kc == config.key_ff => {
                    fast_forward = false;
                    fps_counter.reset();
                }

                // Fast-forward mode (controller)
                Event::ControllerButtonDown { button, .. } if !fast_forward && button == config.ctrl_ff => {
                    fast_forward = true;
                    fps_counter.reset();
                }
                Event::ControllerButtonUp { button, .. } if fast_forward && button == config.ctrl_ff => {
                    fast_forward = false;
                    fps_counter.reset();
                }

                // Pause
                Event::KeyDown {
                    keycode: Some(Keycode::P),
                    ..
                } => {
                    paused = !paused;
                }

                // Outline
                Event::KeyDown {
                    keycode: Some(Keycode::O),
                    ..
                } => {
                    outline = !outline;
                }

                // Save state
                Event::KeyDown {
                    keycode: Some(Keycode::K),
                    ..
                } => {
                    let state = gameboy.save().unwrap();
                    std::fs::write(save_state_path, state)
                        .expect("Failed to dump save state to disk");
                }

                // Load state
                Event::KeyDown {
                    keycode: Some(Keycode::L),
                    ..
                } => {
                    let cartridge = get_cartridge(&rom_file, boot_rom);
                    let data = std::fs::read(save_state_path).expect("Save state not found!");
                    gameboy = Gameboy::load(&data, cartridge).unwrap();
                }

                // Controller joypad events
                Event::ControllerButtonDown { button, .. } => {
                    if let Some(input) = controller_button_to_joypad_input(button, &config) {
                        joypad_events.push(JoypadEvent::Down(input));
                    }
                }
                Event::ControllerButtonUp { button, .. } => {
                    if let Some(input) = controller_button_to_joypad_input(button, &config) {
                        joypad_events.push(JoypadEvent::Up(input));
                    }
                }

                // Keyboard joypad events
                Event::KeyDown { keycode, .. } => {
                    if let Some(kc) = keycode {
                        if let Some(input) = keycode_to_joypad_input(kc, &config) {
                            joypad_events.push(JoypadEvent::Down(input));
                        }
                    }
                }
                Event::KeyUp { keycode, .. } => {
                    if let Some(kc) = keycode {
                        if let Some(input) = keycode_to_joypad_input(kc, &config) {
                            joypad_events.push(JoypadEvent::Up(input));
                        }
                    }
                }

                // Controller device events (hotplug support)
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Ok(c) = controller_subsystem.open(which as u32) {
                        _controllers.push(c);
                    }
                }
                Event::ControllerDeviceRemoved { .. } => {
                    _controllers.clear();
                }

                // Window events: track minimize / focus to avoid SDL render errors on alt-tab
                Event::Window { win_event, .. } => {
                    match win_event {
                        WindowEvent::Minimized | WindowEvent::FocusLost => {
                            minimized = true;
                            if verbose {
                                eprintln!("[DEBUG] Window minimized / focus lost");
                            }
                        }
                        WindowEvent::Restored | WindowEvent::FocusGained | WindowEvent::Exposed => {
                            minimized = false;
                            if verbose {
                                eprintln!("[DEBUG] Window restored / focus gained");
                            }
                            // Re-create the texture since the GPU context may have been lost
                            match texture_creator.create_texture(
                                Some(PixelFormatEnum::ARGB8888),
                                TextureAccess::Streaming,
                                LCD_WIDTH as u32,
                                LCD_HEIGHT as u32,
                            ) {
                                Ok(t) => texture = t,
                                Err(e) => eprintln!("[WARN] Failed to re-create texture: {}", e),
                            }
                        }
                        _ => {}
                    }
                }

                _ => (),
            }
        }

        if !paused && !minimized {
            // Render a single frame; if the GPU context was lost, re-create the texture
            if let Err(e) = handle_frame(
                &mut gameboy,
                &mut canvas,
                &mut texture,
                &mut joypad_events,
                outline,
            ) {
                eprintln!("[WARN] Render error: {} (re-creating texture)", e);
                // Re-create the texture and try once more next frame
                match texture_creator.create_texture(
                    Some(PixelFormatEnum::ARGB8888),
                    TextureAccess::Streaming,
                    LCD_WIDTH as u32,
                    LCD_HEIGHT as u32,
                ) {
                    Ok(t) => texture = t,
                    Err(e2) => eprintln!("[WARN] Texture re-creation also failed: {}", e2),
                }
            }

            // Drain and queue audio samples (or silence to prevent underruns)
            let samples = gameboy.drain_audio();
            if fast_forward {
                audio_device.clear();
            }
            if !samples.is_empty() {
                // Drop queued audio if it's gotten too far ahead to prevent
                // growing latency from the emulator running slightly fast.
                let queued = audio_device.size() as usize;
                if queued > max_queued_bytes {
                    audio_device.clear();
                }
                let _ = audio_device.queue_audio(&samples);
            } else {
                let silence = [0i16; 739 * 2];
                let _ = audio_device.queue_audio(&silence);
            }
            audio_device.resume();

            // If state needs to be persisted, do this at the end of each frame
            if gameboy.is_persist_required() {
                let state = gameboy.persist().expect("Failed to persist state");

                if let Some(state) = state.ram {
                    ram_persist
                        .as_mut()
                        .unwrap()
                        .seek(SeekFrom::Start(0))
                        .unwrap();
                    ram_persist.as_mut().unwrap().write_all(&state).unwrap();
                }

                if let Some(state) = state.rtc {
                    rtc_persist
                        .as_mut()
                        .unwrap()
                        .seek(SeekFrom::Start(0))
                        .unwrap();
                    rtc_persist.as_mut().unwrap().write_all(&state).unwrap();
                }
            }
        } else {
            audio_device.pause();
        }

        let elapsed = frame_start.elapsed();

        // Sleep for the rest of the frame (unless fast forwarding)
        if !fast_forward && elapsed < frame_duration {
            std::thread::sleep(frame_duration - elapsed);
        }

        let _fps = fps_counter.frame(); // keep counter ticking even if we don't display it
    }
}

fn get_cartridge(path: &PathBuf, boot_rom: bool) -> Cartridge {
    let data = std::fs::read(path).expect("Failed to open ROM file");
    let cartridge = Cartridge::from_bytes(data, boot_rom);
    cartridge
}

fn main() {
    let mut verbose = false;
    let mut rom_file: Option<PathBuf> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--debug" | "-d" => verbose = true,
            _ => {
                if rom_file.is_none() {
                    rom_file = Some(PathBuf::from(arg));
                } else {
                    eprintln!("Usage: gbcemu [--debug|-d] <rom_file>");
                    std::process::exit(1);
                }
            }
        }
    }

    let rom_file = rom_file.expect("Usage: gbcemu [--debug|-d] <rom_file>");
    gui(rom_file, verbose);
}
