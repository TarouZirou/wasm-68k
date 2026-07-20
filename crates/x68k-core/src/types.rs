//! 公開APIで使用する、プラットフォーム非依存の型。

use serde::{Deserialize, Serialize};

/// エミュレートするX68000本体。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MachineModel {
    /// 初代相当、MC68000 10MHz。
    #[default]
    X68000,
    /// X68000 XVI、MC68000 16MHz。
    X68000Xvi,
    /// X68030、68EC030 25MHz。
    X68030,
}

impl MachineModel {
    /// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
    pub const fn clock_hz(self) -> u32 {
        match self {
            Self::X68000 => 10_000_000,
            Self::X68000Xvi => 16_000_000,
            Self::X68030 => 25_000_000,
        }
    }

    /// 現在の状態または入力から `name` に対応する値を算出し、副作用なく返す。
    pub const fn name(self) -> &'static str {
        match self {
            Self::X68000 => "X68000",
            Self::X68000Xvi => "X68000 XVI",
            Self::X68030 => "X68030",
        }
    }
}

/// マシン生成時の設定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineConfig {
    pub model: MachineModel,
    /// RAM容量。1–12MiBの範囲で1MiB単位に切り上げる。
    pub ram_bytes: usize,
    pub sample_rate: u32,
}

impl Default for MachineConfig {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            model: MachineModel::X68000,
            ram_bytes: 2 * 1024 * 1024,
            sample_rate: 48_000,
        }
    }
}

/// ROMスロット。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RomKind {
    Ipl,
    CharacterGenerator,
    /// 内蔵128KiBまたは拡張ボード8KiBのSCSI ROM。
    Scsi,
}

/// 媒体の接続先。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DriveId {
    Floppy(u8),
    HardDisk(u8),
}

/// 対応媒体形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaFormat {
    Xdf,
    Dim,
    D88,
    Hdf,
}

/// ホストから投入する入力イベント。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    Key { scancode: u8, pressed: bool },
    MouseMove { dx: i16, dy: i16 },
    MouseButton { button: u8, pressed: bool },
    JoystickButton { port: u8, button: u8, pressed: bool },
    JoystickAxis { port: u8, axis: u8, value: i16 },
}

/// レンダラへ渡す表示効果。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoOptions {
    pub crt_enabled: bool,
    pub scanline_strength: f32,
    pub mask_strength: f32,
    pub curvature: f32,
}

impl Default for VideoOptions {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            crt_enabled: false,
            scanline_strength: 0.25,
            mask_strength: 0.12,
            curvature: 0.04,
        }
    }
}

/// 1フレーム実行後の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameResult {
    pub width: u32,
    pub height: u32,
    pub audio_frames: usize,
    pub frame_number: u64,
}
