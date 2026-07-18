//! コアのエラー型。

use crate::{DriveId, MediaFormat, RomKind};

#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error("invalid RAM size: {0} bytes (expected 1-12 MiB)")]
    InvalidRamSize(usize),
    #[error("invalid {kind:?} ROM size: {actual} bytes")]
    InvalidRomSize { kind: RomKind, actual: usize },
    #[error("{kind:?} ROM is not compatible with {model:?}: {reason}")]
    InvalidRomForModel {
        kind: RomKind,
        model: crate::MachineModel,
        reason: String,
    },
    #[error("invalid {format:?} image: {reason}")]
    InvalidMedia { format: MediaFormat, reason: String },
    #[error("{format:?} media cannot be mounted in {drive:?}")]
    MediaDriveMismatch { drive: DriveId, format: MediaFormat },
    #[error("drive is out of range: {0:?}")]
    InvalidDrive(DriveId),
    #[error("drive is empty: {0:?}")]
    EmptyDrive(DriveId),
    #[error("media is write protected: {0:?}")]
    WriteProtected(DriveId),
    #[error("invalid save state: {0}")]
    InvalidState(String),
    #[error("save state belongs to {state_model:?}, current model is {current_model:?}")]
    StateModelMismatch {
        state_model: crate::MachineModel,
        current_model: crate::MachineModel,
    },
    #[error("save state ROM/media set does not match the current machine")]
    StateMediaMismatch,
}
