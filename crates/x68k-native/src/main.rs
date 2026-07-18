//! X68000 エミュレータのネイティブデバッグランナー。
//!
//! ブラウザを介さずにコアを高速に動かし、println デバッグやプロファイリングを
//! 可能にする。描画は `x68k-render` (wgpu) を使用する。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, ElementState, MouseButton, WindowEvent};
use winit::event_loop::ControlFlow;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};
use x68k_core::{InputEvent, Machine, MachineConfig};
use x68k_render::Renderer;

struct MidiOutput {
    connection: midir::MidiOutputConnection,
    parser: MidiParser,
}

impl MidiOutput {
    fn new() -> anyhow::Result<Self> {
        let output = midir::MidiOutput::new("wasm-68k")?;
        let port = output
            .ports()
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("MIDI output port is not available"))?;
        let name = output.port_name(&port)?;
        let connection = output
            .connect(&port, "wasm-68k output")
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        log::info!("MIDI output: {name}");
        Ok(Self {
            connection,
            parser: MidiParser::default(),
        })
    }

    fn send(&mut self, bytes: Vec<u8>) {
        for message in self.parser.push(&bytes) {
            if let Err(error) = self.connection.send(&message) {
                log::warn!("MIDI send failed: {error}");
            }
        }
    }
}

#[derive(Default)]
struct MidiParser {
    running_status: Option<u8>,
    message: Vec<u8>,
    expected: usize,
    sysex: bool,
}

impl MidiParser {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut complete = Vec::new();
        for &byte in bytes {
            if byte >= 0xf8 {
                complete.push(vec![byte]);
                continue;
            }
            if self.sysex {
                self.message.push(byte);
                if byte == 0xf7 {
                    complete.push(std::mem::take(&mut self.message));
                    self.sysex = false;
                }
                continue;
            }
            if byte & 0x80 != 0 {
                self.message.clear();
                self.message.push(byte);
                self.expected = midi_message_length(byte);
                self.sysex = byte == 0xf0;
                self.running_status = (byte < 0xf0).then_some(byte);
                if self.expected == 1 {
                    complete.push(std::mem::take(&mut self.message));
                }
                continue;
            }
            if self.message.is_empty()
                && let Some(status) = self.running_status
            {
                self.message.push(status);
                self.expected = midi_message_length(status);
            }
            if !self.message.is_empty() {
                self.message.push(byte);
                if self.message.len() == self.expected {
                    complete.push(std::mem::take(&mut self.message));
                }
            }
        }
        complete
    }
}

fn midi_message_length(status: u8) -> usize {
    match status {
        0x80..=0xbf | 0xe0..=0xef | 0xf2 => 3,
        0xc0..=0xdf | 0xf1 | 0xf3 => 2,
        0xf0 => usize::MAX,
        _ => 1,
    }
}

/// コアが生成したインターリーブ stereo PCM をホストの出力形式へ橋渡しする。
struct AudioOutput {
    queue: Arc<Mutex<VecDeque<f32>>>,
    _sdl: sdl2::Sdl,
    _device: AudioDevice<QueueCallback>,
    sample_rate: u32,
}

struct QueueCallback {
    queue: Arc<Mutex<VecDeque<f32>>>,
    channels: usize,
}

impl AudioCallback for QueueCallback {
    type Channel = f32;

    fn callback(&mut self, output: &mut [f32]) {
        write_audio(output, self.channels, &self.queue);
    }
}

impl AudioOutput {
    fn new() -> anyhow::Result<Self> {
        let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
        let audio = sdl.audio().map_err(anyhow::Error::msg)?;
        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(48_000)));
        let callback_queue = Arc::clone(&queue);
        let desired = AudioSpecDesired {
            freq: Some(48_000),
            channels: Some(2),
            samples: Some(1024),
        };
        let device = audio
            .open_playback(None, &desired, move |spec| QueueCallback {
                queue: callback_queue,
                channels: usize::from(spec.channels),
            })
            .map_err(anyhow::Error::msg)?;
        let sample_rate = device.spec().freq.max(1) as u32;
        log::info!(
            "audio output: {} Hz, {} channels",
            sample_rate,
            device.spec().channels
        );
        device.resume();

        Ok(Self {
            queue,
            _sdl: sdl,
            _device: device,
            sample_rate,
        })
    }

    fn enqueue(&self, samples: Vec<f32>) {
        let Ok(mut queue) = self.queue.lock() else {
            return;
        };
        // 描画停止後の復帰時などに古い音が長く残らないよう、最大1秒に制限する。
        let capacity = self.sample_rate as usize * 2;
        let overflow = queue
            .len()
            .saturating_add(samples.len())
            .saturating_sub(capacity);
        let discard = overflow.min(queue.len());
        queue.drain(..discard);
        queue.extend(samples);
    }
}

fn write_audio(output: &mut [f32], channels: usize, queue: &Mutex<VecDeque<f32>>) {
    let Ok(mut queue) = queue.lock() else {
        output.fill(0.0);
        return;
    };
    for frame in output.chunks_mut(channels) {
        let left = queue.pop_front().unwrap_or(0.0);
        let right = queue.pop_front().unwrap_or(0.0);
        if channels == 1 {
            frame[0] = (left + right) * 0.5;
        } else {
            frame[0] = left;
            frame[1] = right;
            for sample in &mut frame[2..] {
                *sample = (left + right) * 0.5;
            }
        }
    }
}

struct App {
    machine: Machine,
    display_handle: Option<winit::event_loop::OwnedDisplayHandle>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    audio: Option<AudioOutput>,
    midi: Option<MidiOutput>,
    next_frame: Instant,
    pressed_keys: HashMap<KeyCode, u8>,
    pressed_mouse_buttons: HashSet<u8>,
}

impl App {
    fn release_all_inputs(&mut self) {
        let scancodes = self
            .pressed_keys
            .drain()
            .map(|(_, scan)| scan)
            .collect::<HashSet<_>>();
        for scancode in scancodes {
            self.machine.input(InputEvent::Key {
                scancode,
                pressed: false,
            });
        }
        for button in self.pressed_mouse_buttons.drain() {
            self.machine.input(InputEvent::MouseButton {
                button,
                pressed: false,
            });
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("wasm-68k (native)")
            .with_inner_size(winit::dpi::LogicalSize::new(768.0, 512.0));
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("failed to create window"),
        );

        let size = window.inner_size();
        let display = self
            .display_handle
            .take()
            .map(|handle| Box::new(handle) as Box<dyn wgpu_types::WgpuHasDisplayHandle>);
        let renderer = pollster::block_on(Renderer::new_with_display(
            window.clone(),
            display,
            size.width,
            size.height,
        ))
        .expect("failed to initialize renderer");

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.next_frame = Instant::now();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(false) => self.release_all_inputs(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                self.machine.run_frame();
                let samples = self.machine.drain_audio();
                if let Some(audio) = &self.audio {
                    audio.enqueue(samples);
                }
                let midi = self.machine.drain_midi();
                if let Some(output) = &mut self.midi {
                    output.send(midi);
                }
                let (width, height) = self.machine.screen_dimensions();
                if let Some(renderer) = &mut self.renderer
                    && let Err(error) = renderer.render(self.machine.framebuffer(), width, height)
                {
                    log::error!("render error: {error:#}");
                }
                const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);
                self.next_frame += FRAME;
                let now = Instant::now();
                if now.saturating_duration_since(self.next_frame) > Duration::from_millis(250) {
                    self.next_frame = now + FRAME;
                }
                event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(scancode) = x68k_scancode(code)
                {
                    match event.state {
                        ElementState::Pressed => {
                            self.pressed_keys.insert(code, scancode);
                            // OSのrepeat makeも実機キーボードのtypematicとして渡す。
                            self.machine.input(InputEvent::Key {
                                scancode,
                                pressed: true,
                            });
                        }
                        ElementState::Released => {
                            let Some(scancode) = self.pressed_keys.remove(&code) else {
                                return;
                            };
                            // 左右Shift/Controlは同じX68000キーなので、両方を離すまで
                            // break codeを送らない。
                            if !self.pressed_keys.values().any(|value| *value == scancode) {
                                self.machine.input(InputEvent::Key {
                                    scancode,
                                    pressed: false,
                                });
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    MouseButton::Left => Some(0),
                    MouseButton::Right => Some(1),
                    MouseButton::Middle => Some(2),
                    _ => None,
                };
                if let Some(button) = button {
                    if state == ElementState::Pressed {
                        self.pressed_mouse_buttons.insert(button);
                    } else {
                        self.pressed_mouse_buttons.remove(&button);
                    }
                    self.machine.input(InputEvent::MouseButton {
                        button,
                        pressed: state == ElementState::Pressed,
                    });
                }
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event {
            self.machine.input(InputEvent::MouseMove {
                dx: dx.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
                dy: dy.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
            });
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
        }
    }
}

fn x68k_scancode(code: KeyCode) -> Option<u8> {
    Some(match code {
        KeyCode::Escape => 0x01,
        KeyCode::Digit1 => 0x02,
        KeyCode::Digit2 => 0x03,
        KeyCode::Digit3 => 0x04,
        KeyCode::Digit4 => 0x05,
        KeyCode::Digit5 => 0x06,
        KeyCode::Digit6 => 0x07,
        KeyCode::Digit7 => 0x08,
        KeyCode::Digit8 => 0x09,
        KeyCode::Digit9 => 0x0a,
        KeyCode::Digit0 => 0x0b,
        KeyCode::Minus => 0x0c,
        KeyCode::Equal => 0x0d,
        KeyCode::Backquote | KeyCode::IntlYen => 0x0e,
        KeyCode::Backspace => 0x0f,
        KeyCode::Tab => 0x10,
        KeyCode::KeyQ => 0x11,
        KeyCode::KeyW => 0x12,
        KeyCode::KeyE => 0x13,
        KeyCode::KeyR => 0x14,
        KeyCode::KeyT => 0x15,
        KeyCode::KeyY => 0x16,
        KeyCode::KeyU => 0x17,
        KeyCode::KeyI => 0x18,
        KeyCode::KeyO => 0x19,
        KeyCode::KeyP => 0x1a,
        KeyCode::BracketLeft => 0x1b,
        KeyCode::BracketRight => 0x1c,
        KeyCode::Enter => 0x1d,
        KeyCode::KeyA => 0x1e,
        KeyCode::KeyS => 0x1f,
        KeyCode::KeyD => 0x20,
        KeyCode::KeyF => 0x21,
        KeyCode::KeyG => 0x22,
        KeyCode::KeyH => 0x23,
        KeyCode::KeyJ => 0x24,
        KeyCode::KeyK => 0x25,
        KeyCode::KeyL => 0x26,
        KeyCode::Semicolon => 0x27,
        KeyCode::Quote => 0x28,
        KeyCode::Backslash => 0x29,
        KeyCode::KeyZ => 0x2a,
        KeyCode::KeyX => 0x2b,
        KeyCode::KeyC => 0x2c,
        KeyCode::KeyV => 0x2d,
        KeyCode::KeyB => 0x2e,
        KeyCode::KeyN => 0x2f,
        KeyCode::KeyM => 0x30,
        KeyCode::Comma => 0x31,
        KeyCode::Period => 0x32,
        KeyCode::Slash => 0x33,
        KeyCode::IntlRo => 0x34,
        KeyCode::Space => 0x35,
        KeyCode::Home => 0x36,
        KeyCode::Delete => 0x37,
        KeyCode::PageUp => 0x38,
        KeyCode::PageDown => 0x39,
        KeyCode::End => 0x3a,
        KeyCode::ArrowLeft => 0x3b,
        KeyCode::ArrowUp => 0x3c,
        KeyCode::ArrowRight => 0x3d,
        KeyCode::ArrowDown => 0x3e,
        KeyCode::NumLock => 0x3f,
        KeyCode::NumpadDivide => 0x40,
        KeyCode::NumpadMultiply => 0x41,
        KeyCode::NumpadSubtract => 0x42,
        KeyCode::Numpad7 => 0x43,
        KeyCode::Numpad8 => 0x44,
        KeyCode::Numpad9 => 0x45,
        KeyCode::NumpadAdd => 0x46,
        KeyCode::Numpad4 => 0x47,
        KeyCode::Numpad5 => 0x48,
        KeyCode::Numpad6 => 0x49,
        KeyCode::NumpadEqual => 0x4a,
        KeyCode::Numpad1 => 0x4b,
        KeyCode::Numpad2 => 0x4c,
        KeyCode::Numpad3 => 0x4d,
        KeyCode::NumpadEnter => 0x4e,
        KeyCode::Numpad0 => 0x4f,
        KeyCode::NumpadComma => 0x50,
        KeyCode::NumpadDecimal => 0x51,
        KeyCode::Help => 0x54,
        KeyCode::F11 => 0x55,
        KeyCode::F12 => 0x56,
        KeyCode::F13 => 0x57,
        KeyCode::F14 => 0x58,
        KeyCode::F15 => 0x59,
        KeyCode::KanaMode | KeyCode::Lang3 => 0x5a,
        KeyCode::CapsLock => 0x5d,
        KeyCode::Insert => 0x5e,
        KeyCode::Hiragana | KeyCode::Lang4 => 0x5f,
        KeyCode::Lang5 => 0x60,
        KeyCode::F1 => 0x63,
        KeyCode::F2 => 0x64,
        KeyCode::F3 => 0x65,
        KeyCode::F4 => 0x66,
        KeyCode::F5 => 0x67,
        KeyCode::F6 => 0x68,
        KeyCode::F7 => 0x69,
        KeyCode::F8 => 0x6a,
        KeyCode::F9 => 0x6b,
        KeyCode::F10 => 0x6c,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => 0x70,
        KeyCode::ControlLeft | KeyCode::ControlRight => 0x71,
        KeyCode::PrintScreen | KeyCode::AltLeft => 0x72,
        KeyCode::Pause | KeyCode::AltRight => 0x73,
        _ => return None,
    })
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    #[test]
    fn maps_host_keys_to_x68000_matrix_codes() {
        for (key, scan) in [
            (KeyCode::Enter, 0x1d),
            (KeyCode::KeyA, 0x1e),
            (KeyCode::KeyZ, 0x2a),
            (KeyCode::KeyX, 0x2b),
            (KeyCode::Space, 0x35),
            (KeyCode::ArrowLeft, 0x3b),
            (KeyCode::ArrowUp, 0x3c),
            (KeyCode::ArrowRight, 0x3d),
            (KeyCode::ArrowDown, 0x3e),
            (KeyCode::F1, 0x63),
            (KeyCode::F10, 0x6c),
            (KeyCode::ShiftLeft, 0x70),
            (KeyCode::ControlLeft, 0x71),
        ] {
            assert_eq!(x68k_scancode(key), Some(scan), "{key:?}");
        }
    }

    #[test]
    fn left_and_right_modifiers_share_the_hardware_key() {
        assert_eq!(
            x68k_scancode(KeyCode::ShiftLeft),
            x68k_scancode(KeyCode::ShiftRight)
        );
        assert_eq!(
            x68k_scancode(KeyCode::ControlLeft),
            x68k_scancode(KeyCode::ControlRight)
        );
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let event_loop = EventLoop::new()?;
    let audio = match AudioOutput::new() {
        Ok(audio) => Some(audio),
        Err(error) => {
            log::warn!("audio output disabled: {error:#}");
            None
        }
    };
    let mut config = MachineConfig::default();
    if let Some(audio) = &audio {
        config.sample_rate = audio.sample_rate;
    }
    let midi = match MidiOutput::new() {
        Ok(midi) => Some(midi),
        Err(error) => {
            log::info!("MIDI output disabled: {error:#}");
            None
        }
    };
    let mut app = App {
        machine: Machine::new(config)?,
        // GLES バックエンドでサーフェスを作るために必要 (取り出しは resumed 時)
        display_handle: Some(event_loop.owned_display_handle()),
        window: None,
        renderer: None,
        audio,
        midi,
        next_frame: Instant::now(),
        pressed_keys: HashMap::new(),
        pressed_mouse_buttons: HashSet::new(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
