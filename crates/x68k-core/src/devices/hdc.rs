//! SASI/SCSIハードディスクのコマンド／データ／ステータス各フェーズ。
//!
//! 初代のSASIポートとX68030の内蔵SCSIで共通に使えるコマンドエンジン。
//! 媒体は常にコピーオンライト層を介し、SASIは256-byte、SCSIは512-byte
//! 論理ブロックで扱う。

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::media::MediaImage;
use crate::{DriveId, MediaFormat};

const SASI_BLOCK_SIZE: usize = 256;
const SCSI_BLOCK_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Interface {
    #[default]
    Sasi,
    Scsi,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    #[default]
    BusFree,
    Selected,
    Command,
    DataIn,
    DataOut,
    Status,
    Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Spc {
    registers: [u8; 16],
    transfer_count: u32,
    connected: bool,
    transfer_active: bool,
}

impl Default for Spc {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        let mut registers = [0; 16];
        registers[1] = 0x80; // SCTL reset and disable
        Self {
            registers,
            transfer_count: 0,
            connected: false,
            transfer_active: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Hdc {
    interface: Interface,
    phase: Phase,
    target: u8,
    unit: u8,
    command: Vec<u8>,
    data: VecDeque<u8>,
    write_data: Vec<u8>,
    write_lba: u32,
    write_blocks: u32,
    status: u8,
    sense_key: u8,
    sense_code: u8,
    interrupt: bool,
    spc: Spc,
}

impl Hdc {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&mut self, offset: u32, media: &BTreeMap<DriveId, MediaImage>) -> u8 {
        match offset & 7 {
            1 => self.read_data(media),
            3 => self.bus_status(),
            _ => 0xff,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(
        &mut self,
        offset: u32,
        value: u8,
        media: &mut BTreeMap<DriveId, MediaImage>,
    ) {
        match offset & 7 {
            1 => self.write_data(value, media),
            3 if self.phase == Phase::Selected => self.phase = Phase::Command,
            5 => self.reset(),
            7 if self.phase == Phase::BusFree => {
                self.interface = Interface::Sasi;
                self.target = value.trailing_zeros().min(7) as u8;
                self.phase = Phase::Selected;
                self.command.clear();
                self.interrupt = false;
            }
            _ => {}
        }
    }

    /// `interrupt_pending` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn interrupt_pending(&self) -> bool {
        self.interrupt || self.spc.registers[1] & 1 != 0 && self.spc.registers[4] != 0
    }

    /// 割り込み状態を更新し、CPUと周辺機器のハンドシェイクを進める。
    pub(crate) fn acknowledge(&mut self) {
        self.interrupt = false;
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    pub(crate) fn read_spc(&mut self, offset: u32, media: &BTreeMap<DriveId, MediaImage>) -> u8 {
        if offset & 1 == 0 {
            return 0xff;
        }
        let register = ((offset >> 1) & 0x0f) as usize;
        match register {
            0 => 1 << (self.spc.registers[0] & 7),
            4 => self.spc.registers[4],
            5 => self.spc_psns(),
            6 => {
                let empty = 1;
                let tc_zero = if self.spc.transfer_count == 0 { 4 } else { 0 };
                let transfer = if self.spc.transfer_active { 0x30 } else { 0 };
                let connected = if self.spc.connected { 0x80 } else { 0 };
                empty | tc_zero | transfer | connected
            }
            7 => self.spc.registers[7],
            9 => 0,
            10 => self.spc_read_data(media),
            12 => (self.spc.transfer_count >> 16) as u8,
            13 => (self.spc.transfer_count >> 8) as u8,
            14 => self.spc.transfer_count as u8,
            _ => self.spc.registers[register],
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    pub(crate) fn write_spc(
        &mut self,
        offset: u32,
        value: u8,
        media: &mut BTreeMap<DriveId, MediaImage>,
    ) {
        if offset & 1 == 0 {
            return;
        }
        let register = ((offset >> 1) & 0x0f) as usize;
        match register {
            0 => self.spc.registers[0] = value & 7,
            1 => {
                self.spc.registers[1] = value;
                if value & 0x80 != 0 {
                    self.spc = Spc::default();
                    self.phase = Phase::BusFree;
                }
            }
            2 => self.spc_command(value, media),
            4 => self.spc.registers[4] &= !value,
            5 | 8 => self.spc.registers[register] = value,
            10 => self.spc_write_data(value, media),
            11 => self.spc.registers[11] = value,
            12 => {
                self.spc.transfer_count =
                    (self.spc.transfer_count & 0x0000_ffff) | (u32::from(value) << 16)
            }
            13 => {
                self.spc.transfer_count =
                    // Transfer count is a 24-bit register split across
                    // registers 12, 13 and 14. Preserve the programmed
                    // high and low bytes while replacing the middle byte.
                    (self.spc.transfer_count & 0x00ff_00ff) | (u32::from(value) << 8)
            }
            14 => {
                self.spc.transfer_count = (self.spc.transfer_count & 0x00ff_ff00) | u32::from(value)
            }
            _ => self.spc.registers[register] = value,
        }
    }

    /// MB89352へ与えられたコマンドを解釈し、SCSIフェーズ遷移を開始する。
    fn spc_command(&mut self, value: u8, media: &mut BTreeMap<DriveId, MediaImage>) {
        self.spc.registers[2] = value;
        match value & 0xe0 {
            0x00 => self.spc.transfer_active = false,
            0x20 => {
                let initiator = 1u8 << (self.spc.registers[0] & 7);
                let targets = self.spc.registers[11] & !initiator;
                if targets == 0 {
                    self.spc.registers[4] |= 0x04;
                    return;
                }
                self.target = targets.trailing_zeros().min(7) as u8;
                self.interface = Interface::Scsi;
                self.phase = Phase::Command;
                self.command.clear();
                self.interrupt = false;
                self.spc.connected = true;
                self.spc.registers[4] |= 0x10;
            }
            0x80 => {
                if self.spc.connected {
                    self.spc.transfer_active = true;
                    if self.spc.transfer_count == 0 {
                        self.spc_finish_transfer();
                    }
                }
            }
            0xc0 => self.spc.transfer_active = false,
            _ => {}
        }
        let _ = media;
    }

    /// 現在のSCSIバスフェーズをMB89352 PSNSレジスタ形式で返す。
    fn spc_psns(&self) -> u8 {
        if !self.spc.connected {
            return 0;
        }
        0x88 | match self.phase {
            Phase::Command => 0x02,
            Phase::DataIn => 0x01,
            Phase::DataOut => 0,
            Phase::Status => 0x03,
            Phase::Message => 0x07,
            _ => 0,
        }
    }

    /// SCSI Data Inフェーズの次のバイトを媒体から読み取る。
    fn spc_read_data(&mut self, media: &BTreeMap<DriveId, MediaImage>) -> u8 {
        if !self.spc.transfer_active
            || !matches!(self.phase, Phase::DataIn | Phase::Status | Phase::Message)
        {
            return self.spc.registers[10];
        }
        let value = self.read_data(media);
        self.spc.registers[10] = value;
        self.spc_advance_transfer();
        value
    }

    /// SCSI Data Outフェーズのバイトを媒体オーバーレイへ書き込む。
    fn spc_write_data(&mut self, value: u8, media: &mut BTreeMap<DriveId, MediaImage>) {
        self.spc.registers[10] = value;
        if !self.spc.transfer_active || !matches!(self.phase, Phase::Command | Phase::DataOut) {
            return;
        }
        self.write_data(value, media);
        self.spc_advance_transfer();
    }

    /// SCSI転送の残量とフェーズを更新し、完了時は次フェーズへ移る。
    fn spc_advance_transfer(&mut self) {
        self.spc.transfer_count = self.spc.transfer_count.saturating_sub(1);
        if self.spc.transfer_count == 0 {
            self.spc_finish_transfer();
        }
        if self.phase == Phase::BusFree {
            self.spc.connected = false;
        }
    }

    /// SCSI転送を完了し、状態・IRQ・フェーズを確定する。
    fn spc_finish_transfer(&mut self) {
        self.spc.transfer_active = false;
        self.spc.registers[4] |= 0x10;
    }

    /// 内部状態をリセットし、関連する周辺機器を起動直後の状態へ戻す。
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// 現在のSASIバスフェーズと信号線をホストが読むステータス値へ変換する。
    fn bus_status(&self) -> u8 {
        let mut status = 0;
        if self.phase != Phase::BusFree {
            status |= 0x08; // BSY
        }
        if matches!(
            self.phase,
            Phase::Command | Phase::DataIn | Phase::DataOut | Phase::Status | Phase::Message
        ) {
            status |= 0x80; // REQ
        }
        if matches!(self.phase, Phase::Command | Phase::Status | Phase::Message) {
            status |= 0x10; // C/D
        }
        if matches!(self.phase, Phase::DataIn | Phase::Status | Phase::Message) {
            status |= 0x20; // I/O
        }
        if self.phase == Phase::Message {
            status |= 0x40;
        }
        status
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_data(&mut self, media: &BTreeMap<DriveId, MediaImage>) -> u8 {
        match self.phase {
            Phase::DataIn => {
                let value = self.data.pop_front().unwrap_or(0);
                if self.data.is_empty() {
                    self.phase = Phase::Status;
                    self.interrupt = true;
                }
                value
            }
            Phase::Status => {
                self.phase = Phase::Message;
                self.status
            }
            Phase::Message => {
                self.phase = Phase::BusFree;
                self.interrupt = false;
                0
            }
            // 選択直後の媒体存在確認を行うための互換動作。
            Phase::Selected if !self.has_media(media) => 0xff,
            _ => 0xff,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_data(&mut self, value: u8, media: &mut BTreeMap<DriveId, MediaImage>) {
        match self.phase {
            Phase::Command => {
                self.command.push(value);
                if self.command.len() == command_length(self.command[0]) {
                    self.execute_command(media);
                }
            }
            Phase::DataOut => {
                self.write_data.push(value);
                let expected = self.write_blocks as usize * self.block_size();
                if self.write_data.len() >= expected {
                    self.commit_write(media);
                }
            }
            _ => {}
        }
    }

    /// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
    fn execute_command(&mut self, media: &mut BTreeMap<DriveId, MediaImage>) {
        self.status = 0;
        self.interrupt = false;
        self.data.clear();
        self.unit = (self.command.get(1).copied().unwrap_or(0) >> 5) & 1;
        match self.command[0] {
            0x00 | 0x01 | 0x0b | 0x1b | 0x1e | 0x2f | 0x35 => {
                if self.has_media(media) {
                    self.finish_ok();
                } else {
                    self.fail(0x02, 0x3a);
                }
            }
            0x03 => {
                let allocation = usize::from(self.command.get(4).copied().unwrap_or(4).max(4));
                let mut sense = vec![0; allocation.min(18)];
                if sense.len() >= 4 {
                    sense[0] = 0x70;
                    sense[2] = self.sense_key;
                    sense[3] = 0;
                }
                if sense.len() >= 14 {
                    sense[7] = 10;
                    sense[12] = self.sense_code;
                }
                self.sense_key = 0;
                self.sense_code = 0;
                self.start_data_in(sense);
            }
            0x08 | 0x28 => {
                let (lba, blocks) = command_range(&self.command);
                self.read_blocks(media, lba, blocks);
            }
            0x0a | 0x2a => {
                let (lba, blocks) = command_range(&self.command);
                if !self.has_media(media) {
                    self.fail(0x02, 0x3a);
                } else if self
                    .selected_media(media)
                    .is_some_and(|image| image.write_protected)
                {
                    self.fail(0x07, 0x27);
                } else {
                    self.write_lba = lba;
                    self.write_blocks = blocks;
                    self.write_data.clear();
                    if blocks == 0 {
                        self.finish_ok();
                    } else {
                        self.phase = Phase::DataOut;
                    }
                }
            }
            0x12 => {
                let allocation = usize::from(self.command.get(4).copied().unwrap_or(36));
                let mut inquiry = vec![0; 36];
                inquiry[0] = 0;
                inquiry[1] = 0;
                inquiry[2] = 1;
                inquiry[3] = 1;
                inquiry[4] = 31;
                inquiry[8..16].copy_from_slice(b"WASM68K ");
                inquiry[16..32].copy_from_slice(b"VIRTUAL HDD     ");
                inquiry[32..36].copy_from_slice(b"0001");
                inquiry.truncate(allocation.min(inquiry.len()));
                self.start_data_in(inquiry);
            }
            0x1a => {
                // MODE SENSE(6): write-protect bitと512/256-byte block descriptorを返す。
                let allocation = usize::from(self.command.get(4).copied().unwrap_or(12));
                let mut mode = vec![0; 12];
                mode[0] = 11;
                mode[2] = if self
                    .selected_media(media)
                    .is_some_and(|image| image.write_protected)
                {
                    0x80
                } else {
                    0
                };
                mode[3] = 8;
                if let Some(image) = self.selected_media(media) {
                    let blocks = image.len() / self.block_size();
                    let blocks = blocks.min(0x00ff_ffff);
                    mode[5..8].copy_from_slice(&(blocks as u32).to_be_bytes()[1..]);
                }
                mode[9..12].copy_from_slice(&(self.block_size() as u32).to_be_bytes()[1..]);
                mode.truncate(allocation.min(mode.len()));
                self.start_data_in(mode);
            }
            0x25 => {
                let Some(image) = self.selected_media(media) else {
                    self.fail(0x02, 0x3a);
                    return;
                };
                let block_size = self.block_size();
                let blocks = image.len() / block_size;
                let last = blocks.saturating_sub(1).min(u32::MAX as usize) as u32;
                let mut result = Vec::with_capacity(8);
                result.extend_from_slice(&last.to_be_bytes());
                result.extend_from_slice(&(block_size as u32).to_be_bytes());
                self.start_data_in(result);
            }
            _ => self.fail(0x05, 0x20),
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_blocks(&mut self, media: &BTreeMap<DriveId, MediaImage>, lba: u32, blocks: u32) {
        let Some(image) = self.selected_media(media) else {
            self.fail(0x02, 0x3a);
            return;
        };
        let block_size = self.block_size();
        // CDB values are guest-controlled.  Keep the multiplication checked so
        // malformed READ(6/10) requests cannot wrap a 32-bit Wasm `usize` (or
        // panic in a debug/native test build) before the bounds check below.
        let Some(start) = (lba as usize).checked_mul(block_size) else {
            self.fail(0x05, 0x21);
            return;
        };
        let Some(length) = (blocks as usize).checked_mul(block_size) else {
            self.fail(0x05, 0x21);
            return;
        };
        if start
            .checked_add(length)
            .is_none_or(|end| end > image.len())
        {
            self.fail(0x05, 0x21);
            return;
        }
        let bytes: Option<Vec<u8>> = (start..start + length)
            .map(|offset| image.read(offset as u64))
            .collect();
        if let Some(bytes) = bytes {
            self.start_data_in(bytes);
        } else {
            self.fail(0x03, 0x11);
        }
    }

    /// `commit_write` に必要な状態遷移を実行し、関連するデバイスと入出力を更新する。
    fn commit_write(&mut self, media: &mut BTreeMap<DriveId, MediaImage>) {
        let block_size = self.block_size();
        let Some(start) = (self.write_lba as usize).checked_mul(block_size) else {
            self.fail(0x05, 0x21);
            return;
        };
        let Some(expected) = (self.write_blocks as usize).checked_mul(block_size) else {
            self.fail(0x05, 0x21);
            return;
        };
        let Some(image) = self.selected_media_mut(media) else {
            self.fail(0x02, 0x3a);
            return;
        };
        if start
            .checked_add(expected)
            .is_none_or(|end| end > image.len())
        {
            self.fail(0x05, 0x21);
            return;
        }
        let ok = self.write_data[..expected]
            .iter()
            .copied()
            .enumerate()
            .all(|(index, value)| image.write((start + index) as u64, value));
        if ok {
            self.finish_ok();
        } else {
            self.fail(0x03, 0x0c);
        }
    }

    /// 対象機能の実行状態を切り替え、関連リソースを整合させる。
    fn start_data_in(&mut self, bytes: Vec<u8>) {
        self.data = bytes.into();
        self.phase = if self.data.is_empty() {
            Phase::Status
        } else {
            Phase::DataIn
        };
        self.interrupt = self.phase == Phase::Status;
    }

    /// `finish_ok` に対応する完了またはエラー状態を構築し、関連する要求線とIRQを更新する。
    fn finish_ok(&mut self) {
        self.status = 0;
        self.phase = Phase::Status;
        self.interrupt = true;
    }

    /// `fail` に対応する完了またはエラー状態を構築し、関連する要求線とIRQを更新する。
    fn fail(&mut self, key: u8, code: u8) {
        self.status = 0x02;
        self.sense_key = key;
        self.sense_code = code;
        self.phase = Phase::Status;
        self.interrupt = true;
    }

    /// 現在の状態または入力から `drive` に対応する値を算出し、副作用なく返す。
    fn drive(&self) -> DriveId {
        DriveId::HardDisk(self.target)
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    fn selected_media<'a>(
        &self,
        media: &'a BTreeMap<DriveId, MediaImage>,
    ) -> Option<&'a MediaImage> {
        if self.unit != 0 {
            return None;
        }
        media
            .get(&self.drive())
            .filter(|image| image.format == MediaFormat::Hdf)
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    fn selected_media_mut<'a>(
        &self,
        media: &'a mut BTreeMap<DriveId, MediaImage>,
    ) -> Option<&'a mut MediaImage> {
        if self.unit != 0 {
            return None;
        }
        media
            .get_mut(&self.drive())
            .filter(|image| image.format == MediaFormat::Hdf)
    }

    /// `has_media` の条件が現在成立しているかを、副作用なく判定して返す。
    fn has_media(&self, media: &BTreeMap<DriveId, MediaImage>) -> bool {
        self.selected_media(media).is_some()
    }

    /// 現在のレジスタ値または入力から `block_size` に対応する描画・転送情報を算出して返す。
    fn block_size(&self) -> usize {
        match self.interface {
            Interface::Sasi => SASI_BLOCK_SIZE,
            Interface::Scsi => SCSI_BLOCK_SIZE,
        }
    }
}

/// FDCコマンド種別から必要なパラメータ数を返す。
fn command_length(opcode: u8) -> usize {
    match opcode >> 5 {
        0 => 6,
        1 | 2 => 10,
        5 => 12,
        _ => 6,
    }
}

/// 現在のレジスタ値または入力から `command_range` に対応する描画・転送情報を算出して返す。
fn command_range(command: &[u8]) -> (u32, u32) {
    if command.len() >= 10 {
        let lba = u32::from_be_bytes([command[2], command[3], command[4], command[5]]);
        let blocks = u32::from(u16::from_be_bytes([command[7], command[8]]));
        (lba, blocks)
    } else {
        let lba = (u32::from(command[1] & 0x1f) << 16)
            | (u32::from(command[2]) << 8)
            | u32::from(command[3]);
        let blocks = if command[4] == 0 {
            256
        } else {
            u32::from(command[4])
        };
        (lba, blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    fn select(hdc: &mut Hdc, media: &mut BTreeMap<DriveId, MediaImage>) {
        hdc.write(7, 1, media);
        hdc.write(3, 0, media);
    }

    #[test]
    /// `sasi_read_and_write_use_overlay` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn sasi_read_and_write_use_overlay() {
        let mut media = BTreeMap::new();
        media.insert(
            DriveId::HardDisk(0),
            MediaImage::parse(MediaFormat::Hdf, &vec![0; SASI_BLOCK_SIZE * 4], false).unwrap(),
        );
        let mut hdc = Hdc::default();
        select(&mut hdc, &mut media);
        for byte in [0x0a, 0, 0, 1, 1, 0] {
            hdc.write(1, byte, &mut media);
        }
        for value in 0..SASI_BLOCK_SIZE {
            hdc.write(1, value as u8, &mut media);
        }
        assert_eq!(hdc.read(1, &media), 0);
        assert_eq!(hdc.read(1, &media), 0);

        select(&mut hdc, &mut media);
        for byte in [0x08, 0, 0, 1, 1, 0] {
            hdc.write(1, byte, &mut media);
        }
        for expected in 0..SASI_BLOCK_SIZE {
            assert_eq!(hdc.read(1, &media), expected as u8);
        }
        assert!(
            media[&DriveId::HardDisk(0)]
                .read(SASI_BLOCK_SIZE as u64)
                .is_some()
        );
    }

    #[test]
    /// `scsi_inquiry_and_capacity` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn scsi_inquiry_and_capacity() {
        let mut media = BTreeMap::new();
        media.insert(
            DriveId::HardDisk(0),
            MediaImage::parse(MediaFormat::Hdf, &vec![0; SCSI_BLOCK_SIZE * 8], true).unwrap(),
        );
        let mut hdc = Hdc::default();
        select(&mut hdc, &mut media);
        for byte in [0x12, 0, 0, 0, 36, 0] {
            hdc.write(1, byte, &mut media);
        }
        let inquiry: Vec<_> = (0..36).map(|_| hdc.read(1, &media)).collect();
        assert_eq!(&inquiry[8..16], b"WASM68K ");
    }

    #[test]
    /// `scsi_capacity_mode_sense_and_read_use_512_byte_blocks` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn scsi_capacity_mode_sense_and_read_use_512_byte_blocks() {
        let mut image = vec![0; SCSI_BLOCK_SIZE * 4];
        image[SCSI_BLOCK_SIZE] = 0x7b;
        let mut media = BTreeMap::from([(
            DriveId::HardDisk(0),
            MediaImage::parse(MediaFormat::Hdf, &image, true).unwrap(),
        )]);
        let mut hdc = Hdc::default();

        select(&mut hdc, &mut media);
        hdc.interface = Interface::Scsi;
        for byte in [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
            hdc.write(1, byte, &mut media);
        }
        let capacity: Vec<_> = (0..8).map(|_| hdc.read(1, &media)).collect();
        assert_eq!(u32::from_be_bytes(capacity[0..4].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_be_bytes(capacity[4..8].try_into().unwrap()),
            SCSI_BLOCK_SIZE as u32
        );
        hdc.read(1, &media); // status
        hdc.read(1, &media); // message

        select(&mut hdc, &mut media);
        hdc.interface = Interface::Scsi;
        for byte in [0x1a, 0, 0, 0, 12, 0] {
            hdc.write(1, byte, &mut media);
        }
        let mode: Vec<_> = (0..12).map(|_| hdc.read(1, &media)).collect();
        assert_eq!(mode[2] & 0x80, 0x80);
        assert_eq!(u32::from_be_bytes([0, mode[9], mode[10], mode[11]]), 512);
        hdc.read(1, &media);
        hdc.read(1, &media);

        select(&mut hdc, &mut media);
        hdc.interface = Interface::Scsi;
        for byte in [0x28, 0, 0, 0, 0, 1, 0, 0, 1, 0] {
            hdc.write(1, byte, &mut media);
        }
        assert_eq!(hdc.read(1, &media), 0x7b);
        for _ in 1..SCSI_BLOCK_SIZE {
            hdc.read(1, &media);
        }
        assert_eq!(hdc.read(1, &media), 0); // status
    }

    #[test]
    /// `scsi_target_maps_one_to_one_to_public_hard_disk_id` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn scsi_target_maps_one_to_one_to_public_hard_disk_id() {
        let mut media = BTreeMap::from([(
            DriveId::HardDisk(1),
            MediaImage::parse(MediaFormat::Hdf, &vec![0x5a; SASI_BLOCK_SIZE * 2], true).unwrap(),
        )]);
        let mut hdc = Hdc::default();
        hdc.write(7, 0b0000_0010, &mut media);
        hdc.write(3, 0, &mut media);
        for byte in [0x08, 0, 0, 0, 1, 0] {
            hdc.write(1, byte, &mut media);
        }
        assert_eq!(hdc.read(1, &media), 0x5a);
    }

    #[test]
    /// `malformed_large_cdb_range_returns_check_condition_without_panicking` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn malformed_large_cdb_range_returns_check_condition_without_panicking() {
        let mut media = BTreeMap::from([(
            DriveId::HardDisk(0),
            MediaImage::parse(MediaFormat::Hdf, &vec![0; SASI_BLOCK_SIZE * 2], true).unwrap(),
        )]);
        let mut hdc = Hdc::default();
        select(&mut hdc, &mut media);

        // READ(10) with the largest 32-bit LBA.  On wasm32, an unchecked
        // `lba * block_size` used to wrap before the image bounds check.
        for byte in [0x28, 0, 0xff, 0xff, 0xff, 0xff, 0, 0, 1, 0] {
            hdc.write(1, byte, &mut media);
        }
        assert_eq!(hdc.read(1, &media), 0x02); // CHECK CONDITION status
        assert_eq!(hdc.read(1, &media), 0); // MESSAGE IN
    }

    /// MB89352の転送要求を現在のSCSIフェーズに従って進める。
    fn spc_transfer(
        hdc: &mut Hdc,
        media: &mut BTreeMap<DriveId, MediaImage>,
        phase: u8,
        bytes: usize,
    ) {
        hdc.write_spc(0x11, phase, media); // PCTL
        hdc.write_spc(0x19, (bytes >> 16) as u8, media);
        hdc.write_spc(0x1b, (bytes >> 8) as u8, media);
        hdc.write_spc(0x1d, bytes as u8, media);
        hdc.write_spc(0x05, 0x84, media); // program transfer
    }

    #[test]
    /// `mb89352_program_transfer_runs_scsi_command_phases` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn mb89352_program_transfer_runs_scsi_command_phases() {
        let mut media = BTreeMap::from([(
            DriveId::HardDisk(0),
            MediaImage::parse(MediaFormat::Hdf, &vec![0; SCSI_BLOCK_SIZE * 8], true).unwrap(),
        )]);
        let mut hdc = Hdc::default();
        hdc.write_spc(0x01, 7, &mut media); // BDID
        hdc.write_spc(0x03, 1, &mut media); // interrupt enable, leave reset
        hdc.write_spc(0x17, 0x81, &mut media); // TEMP: initiator 7 + target 0
        hdc.write_spc(0x05, 0x20, &mut media); // select
        assert_eq!(hdc.read_spc(0x09, &media) & 0x10, 0x10);
        hdc.write_spc(0x09, 0x10, &mut media);

        spc_transfer(&mut hdc, &mut media, 2, 6);
        for byte in [0x12, 0, 0, 0, 36, 0] {
            hdc.write_spc(0x15, byte, &mut media);
        }
        assert_eq!(hdc.read_spc(0x0b, &media) & 7, 1);

        spc_transfer(&mut hdc, &mut media, 1, 36);
        let inquiry: Vec<_> = (0..36).map(|_| hdc.read_spc(0x15, &media)).collect();
        assert_eq!(&inquiry[8..16], b"WASM68K ");

        spc_transfer(&mut hdc, &mut media, 3, 1);
        assert_eq!(hdc.read_spc(0x15, &media), 0);
        spc_transfer(&mut hdc, &mut media, 7, 1);
        assert_eq!(hdc.read_spc(0x15, &media), 0);
        assert_eq!(hdc.read_spc(0x0b, &media), 0);
    }

    #[test]
    /// `mb89352_transfer_count_middle_byte_can_be_reprogrammed` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn mb89352_transfer_count_middle_byte_can_be_reprogrammed() {
        let mut hdc = Hdc::default();
        let mut media = BTreeMap::new();

        // Program 0x12_34_56, then replace only the middle byte. A
        // transfer-count register write must clear the previous value,
        // rather than ORing the two values together.
        hdc.write_spc(0x19, 0x12, &mut media);
        assert_eq!(hdc.spc.transfer_count, 0x12_00_00);
        hdc.write_spc(0x1b, 0x34, &mut media);
        assert_eq!(hdc.spc.transfer_count, 0x12_34_00);
        hdc.write_spc(0x1d, 0x56, &mut media);
        assert_eq!(hdc.spc.transfer_count, 0x12_34_56);
        hdc.write_spc(0x1b, 0xa0, &mut media);
        assert_eq!(hdc.spc.transfer_count, 0x12_a0_56);
    }
}
