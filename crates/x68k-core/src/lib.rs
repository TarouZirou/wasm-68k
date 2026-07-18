//! X68000 emulation core (platform independent).
//!
//! このクレートはプラットフォームに依存しないエミュレーションコアを提供する。
//! 描画は `x68k-render`、フロントエンドは `x68k-native` / `x68k-wasm` が担当する。

mod bus;
pub mod color;
mod devices;
mod error;
mod machine;
mod media;
mod scheduler;
mod state;
mod types;

pub use error::MachineError;
pub use machine::Machine;
pub use types::{
    DriveId, FrameResult, InputEvent, MachineConfig, MachineModel, MediaFormat, RomKind,
    VideoOptions,
};

/// X68000 の最大画面サイズ (ピクセル)。
///
/// CRTC の設定次第で最大 1024x1024 までの表示領域を持ちうる。
pub const MAX_SCREEN_WIDTH: u32 = 1024;
pub const MAX_SCREEN_HEIGHT: u32 = 1024;
