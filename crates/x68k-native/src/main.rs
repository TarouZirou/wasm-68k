//! X68000 エミュレータのネイティブデバッグランナー。
//!
//! ブラウザを介さずにコアを高速に動かし、println デバッグやプロファイリングを
//! 可能にする。描画は `x68k-render` (wgpu) を使用する。

mod keyboard;

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, ElementState, MouseButton, WindowEvent};
use winit::event_loop::ControlFlow;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{Window, WindowId};
use x68k_core::{InputEvent, Machine, MachineConfig};
use x68k_render::Renderer;

use crate::keyboard::KeyboardState;

struct MidiOutput {
    connection: midir::MidiOutputConnection,
    parser: MidiParser,
}

impl MidiOutput {
    /// 必要な初期値と依存オブジェクトを設定し、利用可能なインスタンスを構築する。
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

    /// MIDIメッセージを選択済みのネイティブ出力ポートへ送信する。
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
    /// 入力を処理待ちキューへ追加し、後続処理で利用できるようにする。
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

/// MIDIステータスバイトからメッセージ全体のバイト数を返す。
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

    /// MIDI出力コールバックをコアへ登録し、生成メッセージを転送する。
    fn callback(&mut self, output: &mut [f32]) {
        write_audio(output, self.channels, &self.queue);
    }
}

impl AudioOutput {
    /// 必要な初期値と依存オブジェクトを設定し、利用可能なインスタンスを構築する。
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

    /// 入力を処理待ちキューへ追加し、後続処理で利用できるようにする。
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

/// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
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
    keyboard: KeyboardState,
    pressed_mouse_buttons: HashSet<u8>,
}

impl App {
    /// 入力イベントを処理し、対応するエミュレータ状態と外部出力を更新する。
    fn release_all_inputs(&mut self) {
        for scancode in self.keyboard.drain() {
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
    /// 対象機能の実行状態を切り替え、関連リソースを整合させる。
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

    /// ウィンドウ入力・リサイズ・再描画要求を各サブシステムへ配送する。
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
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let scancode = match event.state {
                    ElementState::Pressed => self.keyboard.press(code),
                    ElementState::Released => self.keyboard.release(code),
                };
                if let Some(scancode) = scancode {
                    self.machine.input(InputEvent::Key {
                        scancode,
                        pressed: event.state == ElementState::Pressed,
                    });
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

    /// デバイス由来の入力イベントをエミュレータ操作へ変換する。
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

    /// イベント待機前に次のフレーム描画を要求する。
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

/// アプリケーションを初期化し、実行に必要な各コンポーネントを起動する。
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
        keyboard: KeyboardState::default(),
        pressed_mouse_buttons: HashSet::new(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}
