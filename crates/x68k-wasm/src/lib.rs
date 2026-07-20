//! X68000エミュレータのwasm-bindgen API。
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;
use x68k_core::{
    DriveId, InputEvent, Machine, MachineConfig, MachineModel, MediaFormat, RomKind, VideoOptions,
};
use x68k_render::{RenderBackend, Renderer};

#[wasm_bindgen]
pub struct WebX68k {
    machine: Machine,
    renderer: Renderer,
    audio_enabled: bool,
    volume: f32,
    midi_enabled: bool,
    last_audio_peak: f32,
    last_frame_timestamp: Option<f64>,
    frame_accumulator_ms: f64,
    needs_redraw: bool,
}

#[wasm_bindgen]
impl WebX68k {
    /// canvasと機種名からエミュレータを初期化する。
    pub async fn create(canvas: HtmlCanvasElement, model: String) -> Result<WebX68k, JsValue> {
        console_error_panic_hook::set_once();
        let model = parse_model(&model)?;
        let renderer = Renderer::new_for_canvas(canvas.clone(), canvas.width(), canvas.height())
            .await
            .map_err(js_error)?;
        let machine = Machine::new(MachineConfig {
            model,
            // 初代IPLには12MiB構成で媒体なし起動が
            // 致命errorへ入る既知問題がある。また既知gameは9MiBを超えて使わない。
            // 互換性とhost memory消費の両方を優先してWeb版は9MiBとする。
            ram_bytes: 9 * 1024 * 1024,
            ..MachineConfig::default()
        })
        .map_err(js_error)?;
        let mut machine = machine;
        // Web Audioはユーザー操作まで開始できない。その間の無音PCM生成を省き、
        // guestのtimer/IRQだけを進める。
        machine.set_audio_output_enabled(false);
        Ok(Self {
            machine,
            renderer,
            audio_enabled: false,
            volume: 1.0,
            midi_enabled: false,
            last_audio_peak: 0.0,
            last_frame_timestamp: None,
            frame_accumulator_ms: 0.0,
            needs_redraw: true,
        })
    }

    /// 時刻差に対応するエミュレーションを進め、完成フレームをGPUへ転送する。
    pub fn frame(&mut self, timestamp: f64) {
        const FRAME_MS: f64 = 1000.0 / 60.0;
        let mut advanced = false;
        match self.last_frame_timestamp.replace(timestamp) {
            None => {
                // 初回は即座に1フレーム生成し、黒画面のまま待たせない。
                let before = self.machine.frame_count();
                self.machine.run_frame();
                advanced = self.machine.frame_count() != before;
            }
            Some(previous) => {
                // 高リフレッシュレート画面でも実機時間は60Hzで進める。1回の
                // requestAnimationFrameで複数frameを追い掛けると、負荷の高い
                // 25MHz/診断実行でUI threadを占有し続けるため、遅延分は捨てる。
                let elapsed = if timestamp.is_finite() && previous.is_finite() {
                    (timestamp - previous).clamp(0.0, 250.0)
                } else {
                    FRAME_MS
                };
                self.frame_accumulator_ms += elapsed;
                if self.frame_accumulator_ms + f64::EPSILON >= FRAME_MS {
                    let before = self.machine.frame_count();
                    self.machine.run_frame();
                    advanced = self.machine.frame_count() != before;
                    self.frame_accumulator_ms =
                        (self.frame_accumulator_ms - FRAME_MS).min(FRAME_MS);
                }
            }
        }
        // 120/144Hz displayでは同じ60Hz frameを2回以上送らない。texture全体の
        // upload、command encoder生成、presentを省けるためGPU/CPU負荷を抑えられる。
        if !advanced && !self.needs_redraw {
            return;
        }
        let (width, height) = self.machine.screen_dimensions();
        if let Err(error) = self
            .renderer
            .render(self.machine.framebuffer(), width, height)
        {
            web_sys::console::error_1(&JsValue::from_str(&format!("render error: {error:#}")));
        }
        self.needs_redraw = false;
    }

    /// 表示サーフェスを新しい物理サイズへ再構成する。
    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
        self.needs_redraw = true;
    }

    /// 実際に選択されたWebGPUまたはWebGL2バックエンド名を返す。
    pub fn backend_name(&self) -> String {
        match self.renderer.backend() {
            RenderBackend::BrowserWebGpu => "webgpu".to_string(),
            RenderBackend::Gl => "webgl2 (fallback)".to_string(),
            other => format!("{other:?}"),
        }
    }

    /// 動作中のX68000機種プロファイル名を返す。
    pub fn model_name(&self) -> String {
        self.machine.config().model.name().to_string()
    }

    /// 起動後に完了したエミュレーションフレーム数を返す。
    pub fn frame_number(&self) -> u64 {
        self.machine.frame_count()
    }

    /// 現在の論理解像度の横幅をピクセル単位で返す。
    pub fn screen_width(&self) -> u32 {
        self.machine.screen_dimensions().0
    }

    /// 現在の論理解像度の縦幅をピクセル単位で返す。
    pub fn screen_height(&self) -> u32 {
        self.machine.screen_dimensions().1
    }

    /// 入力データを検証して読み込み、対応する実行状態へ反映する。
    pub fn load_rom(&mut self, kind: String, bytes: &[u8]) -> Result<(), JsValue> {
        let kind = match kind.as_str() {
            "ipl" => RomKind::Ipl,
            "cgrom" => RomKind::CharacterGenerator,
            "scsi" => RomKind::Scsi,
            _ => return Err(JsValue::from_str("unknown ROM kind")),
        };
        self.machine.load_rom(kind, bytes).map_err(js_error)
    }

    /// 媒体を検証して対象ドライブへ装着し、アクセス可能な状態にする。
    pub fn mount_media(
        &mut self,
        drive_kind: String,
        drive_number: u8,
        format: String,
        bytes: &[u8],
        write_protected: bool,
    ) -> Result<(), JsValue> {
        self.machine
            .mount_media(
                parse_drive(&drive_kind, drive_number)?,
                parse_format(&format)?,
                bytes,
                write_protected,
            )
            .map_err(js_error)
    }

    /// 対象を切り離し、保持していた媒体または入出力状態を更新する。
    pub fn eject_media(
        &mut self,
        drive_kind: String,
        drive_number: u8,
    ) -> Result<Vec<u8>, JsValue> {
        self.machine
            .eject_media(parse_drive(&drive_kind, drive_number)?)
            .map_err(js_error)
    }

    /// 現在の状態を外部で扱える形式へ変換して出力する。
    pub fn export_media(&self, drive_kind: String, drive_number: u8) -> Result<Vec<u8>, JsValue> {
        self.machine
            .export_media(parse_drive(&drive_kind, drive_number)?)
            .map_err(js_error)
    }

    /// 内部状態をリセットし、関連する周辺機器を起動直後の状態へ戻す。
    pub fn reset(&mut self) {
        self.machine.reset();
        self.last_frame_timestamp = None;
        self.frame_accumulator_ms = 0.0;
        self.needs_redraw = true;
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_paused(&mut self, paused: bool) {
        self.machine.set_paused(paused);
    }

    /// `is_paused` の条件が現在成立しているかを、副作用なく判定して返す。
    pub fn is_paused(&self) -> bool {
        self.machine.is_paused()
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_key(&mut self, scancode: u8, pressed: bool) {
        self.machine.input(InputEvent::Key { scancode, pressed });
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_mouse_delta(&mut self, dx: i16, dy: i16) {
        self.machine.input(InputEvent::MouseMove { dx, dy });
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_mouse_button(&mut self, button: u8, pressed: bool) {
        self.machine
            .input(InputEvent::MouseButton { button, pressed });
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_joystick_button(&mut self, port: u8, button: u8, pressed: bool) {
        self.machine.input(InputEvent::JoystickButton {
            port,
            button,
            pressed,
        });
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_joystick_axis(&mut self, port: u8, axis: u8, value: i16) {
        self.machine
            .input(InputEvent::JoystickAxis { port, axis, value });
    }

    /// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
    pub fn drain_audio(&mut self) -> Vec<f32> {
        let mut samples = self.machine.drain_audio();
        for sample in &mut samples {
            *sample *= self.volume;
        }
        if !samples.is_empty() {
            self.last_audio_peak = samples
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        }
        if self.audio_enabled {
            samples
        } else {
            Vec::new()
        }
    }

    /// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
    pub fn drain_midi(&mut self) -> Vec<u8> {
        let bytes = self.machine.drain_midi();
        if self.midi_enabled { bytes } else { Vec::new() }
    }

    /// `set_audio_enabled` の条件が現在成立しているかを、副作用なく判定して返す。
    pub fn set_audio_enabled(&mut self, enabled: bool) {
        self.audio_enabled = enabled;
        self.machine.set_audio_output_enabled(enabled);
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = if volume.is_finite() {
            volume.clamp(0.0, 1.0)
        } else {
            1.0
        };
    }

    /// `set_midi_enabled` の条件が現在成立しているかを、副作用なく判定して返す。
    pub fn set_midi_enabled(&mut self, enabled: bool) {
        self.midi_enabled = enabled;
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub fn set_video_options(
        &mut self,
        crt_enabled: bool,
        scanline_strength: f32,
        mask_strength: f32,
        curvature: f32,
    ) {
        let options = VideoOptions {
            crt_enabled,
            scanline_strength,
            mask_strength,
            curvature,
        };
        self.machine.set_video_options(options);
        self.renderer
            .set_crt_options(crt_enabled, scanline_strength, mask_strength, curvature);
        self.needs_redraw = true;
    }

    /// 現在の状態を外部で扱える形式へ変換して出力する。
    pub fn save_state(&self) -> Result<Vec<u8>, JsValue> {
        self.machine.save_state().map_err(js_error)
    }

    /// 入力データを検証して読み込み、対応する実行状態へ反映する。
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.machine.load_state(bytes).map_err(js_error)?;
        self.needs_redraw = true;
        Ok(())
    }

    /// 現在の状態を外部で扱える形式へ変換して出力する。
    pub fn export_sram(&self) -> Vec<u8> {
        self.machine.sram().to_vec()
    }

    /// 入力データを検証して読み込み、対応する実行状態へ反映する。
    pub fn load_sram(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.machine.load_sram(bytes).map_err(js_error)
    }

    /// 現在の状態や結果を利用者向けの診断情報として提示する。
    pub fn diagnostics(&self) -> String {
        // ROMや媒体そのものは含めず、再現に必要なhashと実行状態だけを出力する。
        let hashes = self
            .machine
            .content_hashes()
            .into_iter()
            .map(|(slot, hash)| format!("{{\"slot\":\"{slot}\",\"sha256\":\"{}\"}}", hex(&hash)))
            .collect::<Vec<_>>()
            .join(",");
        let (width, height) = self.machine.screen_dimensions();
        let (cpu_pc, cpu_sr, cpu_stopped, cpu_sp, exception_pc) = self.machine.cpu_diagnostics();
        let (first_bus_fault, last_bus_fault, bus_fault_count) =
            self.machine.bus_fault_diagnostics();
        let (fdc_commands, fdc_sector_reads, fdc_command, fdc_status, fdc_output) =
            self.machine.fdc_diagnostics();
        let [fdc_st0, fdc_st1, fdc_st2] = self.machine.fdc_result_status();
        format!(
            "{{\"version\":\"{}\",\"build\":\"{}\",\"model\":\"{}\",\"backend\":\"{}\",\"frame\":{},\"width\":{},\"height\":{},\"cpu_pc\":{},\"cpu_sr\":{},\"cpu_stopped\":{},\"cpu_sp\":{},\"exception_pc\":{},\"first_bus_fault\":{},\"last_bus_fault\":{},\"bus_fault_count\":{},\"fdc_commands\":{},\"fdc_sector_reads\":{},\"fdc_command\":{},\"fdc_status\":{},\"fdc_output\":{},\"fdc_st0\":{},\"fdc_st1\":{},\"fdc_st2\":{},\"mouse_buttons\":{},\"frame_sha256\":\"{}\",\"audio_peak\":{},\"content\":[{}]}}",
            env!("CARGO_PKG_VERSION"),
            option_env!("GITHUB_SHA").unwrap_or("local"),
            self.machine.config().model.name(),
            self.backend_name(),
            self.machine.frame_count(),
            width,
            height,
            cpu_pc,
            cpu_sr,
            cpu_stopped,
            cpu_sp,
            exception_pc.map_or("null".to_string(), |pc| pc.to_string()),
            first_bus_fault.map_or("null".to_string(), |address| address.to_string()),
            last_bus_fault.map_or("null".to_string(), |address| address.to_string()),
            bus_fault_count,
            fdc_commands,
            fdc_sector_reads,
            fdc_command,
            fdc_status,
            fdc_output,
            fdc_st0,
            fdc_st1,
            fdc_st2,
            self.machine.mouse_buttons_diagnostics(),
            hex(&self.machine.framebuffer_hash()),
            self.last_audio_peak,
            hashes,
        )
    }
}

/// 16進文字列を検証し、対応するバイト列へ復号する。
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn parse_model(value: &str) -> Result<MachineModel, JsValue> {
    match value.to_ascii_lowercase().as_str() {
        "x68000" | "10mhz" => Ok(MachineModel::X68000),
        "xvi" | "x68000-xvi" | "16mhz" => Ok(MachineModel::X68000Xvi),
        "x68030" | "25mhz" => Ok(MachineModel::X68030),
        _ => Err(JsValue::from_str("unknown machine model")),
    }
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn parse_drive(kind: &str, number: u8) -> Result<DriveId, JsValue> {
    match kind {
        "floppy" => Ok(DriveId::Floppy(number)),
        "hard-disk" => Ok(DriveId::HardDisk(number)),
        _ => Err(JsValue::from_str("unknown drive kind")),
    }
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn parse_format(value: &str) -> Result<MediaFormat, JsValue> {
    match value.to_ascii_lowercase().as_str() {
        // .2HDはXDFと同じ1232KiB raw 2HD imageとして流通している別拡張子。
        "xdf" | "2hd" => Ok(MediaFormat::Xdf),
        "dim" => Ok(MediaFormat::Dim),
        "d88" | "88d" => Ok(MediaFormat::D88),
        "hdf" => Ok(MediaFormat::Hdf),
        _ => Err(JsValue::from_str("unknown media format")),
    }
}

/// Rust側のエラーを内容を失わないJavaScript例外へ変換する。
fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
