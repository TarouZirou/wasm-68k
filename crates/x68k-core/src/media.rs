//! フロッピー/HDDイメージとコピーオンライト層。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{MachineError, MediaFormat};

const XDF_SIZE: usize = 77 * 2 * 8 * 1024;
const DIM_HEADER_SIZE: usize = 256;
const D88_HEADER_SIZE: usize = 0x2b0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MediaImage {
    pub format: MediaFormat,
    #[serde(skip, default)]
    original: Vec<u8>,
    overlay: BTreeMap<u64, u8>,
    pub write_protected: bool,
    digest: [u8; 32],
}

impl MediaImage {
    /// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
    pub fn parse(
        format: MediaFormat,
        bytes: &[u8],
        write_protected: bool,
    ) -> Result<Self, MachineError> {
        match format {
            MediaFormat::Xdf if bytes.len() != XDF_SIZE => {
                return Err(invalid(format, format!("expected {XDF_SIZE} bytes")));
            }
            MediaFormat::Dim => validate_dim(bytes)?,
            MediaFormat::D88 => validate_d88(bytes)?,
            MediaFormat::Hdf if bytes.len() < 256 || bytes.len() % 256 != 0 => {
                return Err(invalid(
                    format,
                    "HDF must contain at least one 256-byte block",
                ));
            }
            _ => {}
        }

        // D88 stores the write-protect flag in bit 4 of the media header.
        // Other bits are drive metadata and must not make a disk read-only.
        let write_protected = write_protected
            || format == MediaFormat::D88 && bytes.get(0x1a).copied().unwrap_or(0) & 0x10 != 0;
        let original = bytes.to_vec();
        let digest: [u8; 32] = Sha256::digest(&original).into();
        Ok(Self {
            format,
            original,
            overlay: BTreeMap::new(),
            write_protected,
            digest,
        })
    }

    /// 元媒体と書込みオーバーレイを含む現在内容のSHA-256を返す。
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }

    /// 媒体またはバッファの現在のバイト長を返す。
    pub fn len(&self) -> usize {
        self.original.len()
    }

    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub fn read(&self, offset: u64) -> Option<u8> {
        let index = usize::try_from(offset).ok()?;
        self.overlay
            .get(&offset)
            .copied()
            .or_else(|| self.original.get(index).copied())
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub fn write(&mut self, offset: u64, value: u8) -> bool {
        let Some(index) = usize::try_from(offset).ok() else {
            return false;
        };
        if self.write_protected || index >= self.original.len() {
            return false;
        }
        if self.original[index] == value {
            self.overlay.remove(&offset);
        } else {
            self.overlay.insert(offset, value);
        }
        true
    }

    /// 現在の状態を外部で扱える形式へ変換して出力する。
    pub fn export(&self) -> Vec<u8> {
        let mut bytes = self.original.clone();
        for (&offset, &value) in &self.overlay {
            if let Ok(index) = usize::try_from(offset) {
                if let Some(byte) = bytes.get_mut(index) {
                    *byte = value;
                }
            }
        }
        bytes
    }

    /// 保存状態へ元媒体データを再接続し、ハッシュ一致を検証する。
    pub fn reattach_original(&mut self, current: &Self) -> bool {
        if self.format != current.format || self.digest != current.digest {
            return false;
        }
        self.original.clone_from(&current.original);
        true
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    pub fn read_sector(
        &self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
    ) -> Option<Vec<u8>> {
        let (offset, length) = self.sector_location(cylinder, head, sector, size_code)?;
        (0..length)
            .map(|index| self.read((offset + index) as u64))
            .collect()
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    pub fn write_sector(
        &mut self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
        bytes: &[u8],
    ) -> bool {
        self.write_sector_deleted(cylinder, head, sector, size_code, bytes, false)
    }

    /// `write_sector_deleted` の条件が現在成立しているかを、副作用なく判定して返す。
    pub fn write_sector_deleted(
        &mut self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
        bytes: &[u8],
        deleted: bool,
    ) -> bool {
        let Some((offset, length)) = self.sector_location(cylinder, head, sector, size_code) else {
            return false;
        };
        if bytes.len() != length || self.write_protected {
            return false;
        }
        let data_written = bytes
            .iter()
            .copied()
            .enumerate()
            .all(|(index, value)| self.write((offset + index) as u64, value));
        if !data_written {
            return false;
        }
        if self.format == MediaFormat::D88 {
            let Some(header) = self.d88_sector_header(cylinder, head, sector, size_code) else {
                return false;
            };
            self.write((header + 7) as u64, if deleted { 0x10 } else { 0 })
        } else {
            true
        }
    }

    /// `sector_deleted` の条件が現在成立しているかを、副作用なく判定して返す。
    pub fn sector_deleted(
        &self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
    ) -> Option<bool> {
        if self.format != MediaFormat::D88 {
            self.sector_location(cylinder, head, sector, size_code)?;
            return Some(false);
        }
        let header = self.d88_sector_header(cylinder, head, sector, size_code)?;
        Some(self.read((header + 7) as u64)? & 0x10 != 0)
    }

    /// 現在の状態または入力から `sector_status` に対応する値を算出し、副作用なく返す。
    pub fn sector_status(&self, cylinder: u8, head: u8, sector: u8, size_code: u8) -> Option<u8> {
        if self.format != MediaFormat::D88 {
            self.sector_location(cylinder, head, sector, size_code)?;
            return Some(0);
        }
        let header = self.d88_sector_header(cylinder, head, sector, size_code)?;
        self.read((header + 8) as u64)
    }

    /// CHRN指定から媒体内セクタのデータ位置と長さを返す。
    fn sector_location(
        &self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
    ) -> Option<(usize, usize)> {
        match self.format {
            MediaFormat::Xdf => {
                if cylinder >= 77 || head >= 2 || !(1..=8).contains(&sector) || size_code != 3 {
                    return None;
                }
                let offset = ((usize::from(cylinder) * 2 + usize::from(head)) * 8
                    + usize::from(sector - 1))
                    * 1024;
                Some((offset, 1024))
            }
            MediaFormat::Dim => self.dim_sector_location(cylinder, head, sector, size_code),
            MediaFormat::D88 => self.d88_sector_location(cylinder, head, sector, size_code),
            MediaFormat::Hdf => None,
        }
    }

    /// DIMジオメトリに従いCHRNをイメージ内オフセットへ変換する。
    fn dim_sector_location(
        &self,
        cylinder: u8,
        head: u8,
        mut sector: u8,
        size_code: u8,
    ) -> Option<(usize, usize)> {
        if self.original.len() == XDF_SIZE {
            if cylinder >= 77 || head >= 2 || !(1..=8).contains(&sector) || size_code != 3 {
                return None;
            }
            return Some((
                ((usize::from(cylinder) * 2 + usize::from(head)) * 8 + usize::from(sector - 1))
                    * 1024,
                1024,
            ));
        }
        let geometry = dim_geometry(*self.original.first()?)?;
        let mut actual_head = head;
        if geometry.kind == 1 && sector > geometry.sectors {
            sector -= geometry.sectors;
        }
        if geometry.kind == 3 {
            actual_head &= 1;
        }
        if cylinder > 84
            || actual_head > 1
            || !(1..=geometry.sectors).contains(&sector)
            || size_code != geometry.size_code
        {
            return None;
        }
        let track = usize::from(cylinder) * 2 + usize::from(actual_head);
        if self.original.get(1 + track).copied()? == 0 {
            return None;
        }
        let preceding = self.original[1..1 + track]
            .iter()
            .filter(|&&flag| flag != 0)
            .count();
        let offset = DIM_HEADER_SIZE
            + preceding * geometry.track_bytes
            + usize::from(sector - 1) * geometry.sector_bytes;
        (offset + geometry.sector_bytes <= self.original.len())
            .then_some((offset, geometry.sector_bytes))
    }

    /// D88の可変トラック表を走査して指定CHRNのデータ位置を返す。
    fn d88_sector_location(
        &self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
    ) -> Option<(usize, usize)> {
        let header = self.d88_sector_header(cylinder, head, sector, size_code)?;
        let length = u16::from_le_bytes([
            self.read((header + 14) as u64)?,
            self.read((header + 15) as u64)?,
        ]) as usize;
        let end = header.checked_add(16)?.checked_add(length)?;
        let declared = self.d88_declared_size()?;
        (end <= declared).then_some((header + 16, length))
    }

    /// D88ヘッダの宣言サイズを検証し、実際にアクセス可能な上限を返す。
    fn d88_declared_size(&self) -> Option<usize> {
        let declared = u32::from_le_bytes(self.original.get(0x1c..0x20)?.try_into().ok()?) as usize;
        (declared >= D88_HEADER_SIZE && declared <= self.original.len()).then_some(declared)
    }

    /// D88セクタヘッダを検証し、CHRN・状態・データ長を復号する。
    fn d88_sector_header(
        &self,
        cylinder: u8,
        head: u8,
        sector: u8,
        size_code: u8,
    ) -> Option<usize> {
        let track = usize::from(cylinder) * 2 + usize::from(head);
        let entry = 0x20 + track * 4;
        let declared = self.d88_declared_size()?;
        if entry.checked_add(4)? > declared {
            return None;
        }
        let mut offset =
            u32::from_le_bytes(self.original.get(entry..entry + 4)?.try_into().ok()?) as usize;
        if offset == 0 || offset < D88_HEADER_SIZE || offset >= declared {
            return None;
        }
        if offset.checked_add(6)? > declared {
            return None;
        }
        let count = u16::from_le_bytes(self.original.get(offset + 4..offset + 6)?.try_into().ok()?);
        for _ in 0..count {
            if offset.checked_add(16)? > declared {
                return None;
            }
            let header = self.original.get(offset..offset + 16)?;
            let length = u16::from_le_bytes(header[14..16].try_into().ok()?) as usize;
            if header[0] == cylinder
                && header[1] == head
                && header[2] == sector
                && header[3] == size_code
            {
                return Some(offset);
            }
            offset = offset.checked_add(16 + length)?;
            if offset > declared {
                return None;
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
struct DimGeometry {
    kind: u8,
    sectors: u8,
    sector_bytes: usize,
    size_code: u8,
    track_bytes: usize,
}

/// DIMヘッダと容量からトラック・面・セクタ構成を判定する。
fn dim_geometry(kind: u8) -> Option<DimGeometry> {
    let (sectors, sector_bytes, size_code) = match kind {
        0 => (8, 1024, 3),
        1 | 3 => (9, 1024, 3),
        2 => (15, 512, 2),
        9 => (18, 512, 2),
        _ => return None,
    };
    Some(DimGeometry {
        kind,
        sectors,
        sector_bytes,
        size_code,
        track_bytes: usize::from(sectors) * sector_bytes,
    })
}

/// 入力または現在状態の妥当性を検査し、問題を呼び出し側へ通知する。
fn validate_dim(bytes: &[u8]) -> Result<(), MachineError> {
    if bytes.len() == XDF_SIZE {
        return Ok(());
    }
    if bytes.len() < DIM_HEADER_SIZE {
        return Err(invalid(MediaFormat::Dim, "header is truncated"));
    }
    let geometry =
        dim_geometry(bytes[0]).ok_or_else(|| invalid(MediaFormat::Dim, "unsupported disk type"))?;
    let tracks = bytes[1..171].iter().filter(|&&flag| flag != 0).count();
    let expected = DIM_HEADER_SIZE
        .checked_add(tracks.saturating_mul(geometry.track_bytes))
        .ok_or_else(|| invalid(MediaFormat::Dim, "image size overflow"))?;
    if bytes.len() != expected {
        return Err(invalid(
            MediaFormat::Dim,
            format!("track flags require {expected} bytes"),
        ));
    }
    Ok(())
}

/// 入力または現在状態の妥当性を検査し、問題を呼び出し側へ通知する。
fn validate_d88(bytes: &[u8]) -> Result<(), MachineError> {
    if bytes.len() < D88_HEADER_SIZE {
        return Err(invalid(MediaFormat::D88, "header is truncated"));
    }
    let declared = u32::from_le_bytes(bytes[0x1c..0x20].try_into().expect("four bytes")) as usize;
    if declared < D88_HEADER_SIZE || declared > bytes.len() {
        return Err(invalid(MediaFormat::D88, "invalid disk size in header"));
    }
    let mut previous = 0usize;
    for chunk in bytes[0x20..D88_HEADER_SIZE].chunks_exact(4) {
        let offset = u32::from_le_bytes(chunk.try_into().expect("four bytes")) as usize;
        if offset == 0 {
            continue;
        }
        if offset < D88_HEADER_SIZE || offset >= declared || offset < previous {
            return Err(invalid(MediaFormat::D88, "invalid track table"));
        }
        previous = offset;
        if offset.checked_add(6).is_none_or(|end| end > declared) {
            return Err(invalid(MediaFormat::D88, "truncated track header"));
        }
        let sectors = u16::from_le_bytes(
            bytes
                .get(offset + 4..offset + 6)
                .ok_or_else(|| invalid(MediaFormat::D88, "truncated sector header"))?
                .try_into()
                .expect("two bytes"),
        ) as usize;
        if sectors == 0 {
            return Err(invalid(MediaFormat::D88, "track has no sectors"));
        }
        let mut cursor = offset;
        for _ in 0..sectors {
            if cursor.checked_add(16).is_none_or(|end| end > declared) {
                return Err(invalid(MediaFormat::D88, "truncated sector header"));
            }
            let header = bytes
                .get(cursor..cursor + 16)
                .ok_or_else(|| invalid(MediaFormat::D88, "truncated sector header"))?;
            let length = u16::from_le_bytes(header[14..16].try_into().expect("two bytes")) as usize;
            if length == 0
                || cursor
                    .checked_add(16 + length)
                    .is_none_or(|end| end > declared)
            {
                return Err(invalid(MediaFormat::D88, "invalid sector length"));
            }
            cursor += 16 + length;
        }
    }
    Ok(())
}

/// 媒体パーサの検証失敗を文脈付きのMachineErrorとして構築する。
fn invalid(format: MediaFormat, reason: impl Into<String>) -> MachineError {
    MachineError::InvalidMedia {
        format,
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `cow_export_does_not_change_source` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn cow_export_does_not_change_source() {
        let bytes = vec![0; XDF_SIZE];
        let mut image = MediaImage::parse(MediaFormat::Xdf, &bytes, false).unwrap();
        assert!(image.write(42, 7));
        assert_eq!(image.read(42), Some(7));
        assert_eq!(bytes[42], 0);
        assert_eq!(image.export()[42], 7);
    }

    #[test]
    /// `offsets_larger_than_wasm_address_space_are_rejected` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn offsets_larger_than_wasm_address_space_are_rejected() {
        let mut image = MediaImage::parse(MediaFormat::Hdf, &[0; 256], false).unwrap();
        let offset = u64::from(u32::MAX) + 1;
        assert_eq!(image.read(offset), None);
        assert!(!image.write(offset, 1));
    }

    #[test]
    /// `hdf_accepts_a_single_sasi_block` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn hdf_accepts_a_single_sasi_block() {
        let image = MediaImage::parse(MediaFormat::Hdf, &[0; 256], false).unwrap();
        assert_eq!(image.len(), 256);
    }

    #[test]
    /// `rejects_short_d88` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn rejects_short_d88() {
        assert!(MediaImage::parse(MediaFormat::D88, &[0; 10], true).is_err());
    }

    /// `one_sector_d88` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn one_sector_d88(protected: bool) -> Vec<u8> {
        let mut bytes = vec![0; D88_HEADER_SIZE + 16 + 128];
        let size = bytes.len() as u32;
        bytes[0x1a] = if protected { 0x10 } else { 0 };
        bytes[0x1c..0x20].copy_from_slice(&size.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&(D88_HEADER_SIZE as u32).to_le_bytes());
        let header = D88_HEADER_SIZE;
        bytes[header..header + 4].copy_from_slice(&[0, 0, 1, 0]);
        bytes[header + 4..header + 6].copy_from_slice(&1u16.to_le_bytes());
        bytes[header + 14..header + 16].copy_from_slice(&128u16.to_le_bytes());
        bytes
    }

    #[test]
    /// `d88_deleted_mark_is_part_of_copy_on_write_overlay` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_deleted_mark_is_part_of_copy_on_write_overlay() {
        let bytes = one_sector_d88(false);
        let mut image = MediaImage::parse(MediaFormat::D88, &bytes, false).unwrap();
        assert_eq!(image.sector_deleted(0, 0, 1, 0), Some(false));
        assert!(image.write_sector_deleted(0, 0, 1, 0, &[0x5a; 128], true));
        assert_eq!(image.sector_deleted(0, 0, 1, 0), Some(true));
        assert_eq!(image.export()[D88_HEADER_SIZE + 7], 0x10);
        assert_eq!(bytes[D88_HEADER_SIZE + 7], 0);

        assert!(image.write_sector(0, 0, 1, 0, &[0xa5; 128]));
        assert_eq!(image.sector_deleted(0, 0, 1, 0), Some(false));
    }

    #[test]
    /// `d88_header_write_protection_is_enforced` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_header_write_protection_is_enforced() {
        let image = MediaImage::parse(MediaFormat::D88, &one_sector_d88(true), false).unwrap();
        assert!(image.write_protected);
    }

    #[test]
    /// `d88_reserved_header_bits_do_not_force_write_protection` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_reserved_header_bits_do_not_force_write_protection() {
        let mut bytes = one_sector_d88(false);
        bytes[0x1a] = 0x01;
        let image = MediaImage::parse(MediaFormat::D88, &bytes, false).unwrap();
        assert!(!image.write_protected);
    }

    #[test]
    /// `d88_declared_size_bounds_track_and_sector_access` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_declared_size_bounds_track_and_sector_access() {
        let mut bytes = one_sector_d88(false);
        // Keep the physical buffer larger than the declared image and point
        // the track table into the trailing bytes. A parser must reject it,
        // rather than exposing data after the D88 image boundary.
        let declared = (D88_HEADER_SIZE + 16) as u32;
        bytes[0x1c..0x20].copy_from_slice(&declared.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&((D88_HEADER_SIZE + 16) as u32).to_le_bytes());
        assert!(MediaImage::parse(MediaFormat::D88, &bytes, false).is_err());
    }

    #[test]
    /// `d88_sector_status_is_preserved_for_fdc_error_reporting` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_sector_status_is_preserved_for_fdc_error_reporting() {
        let mut bytes = one_sector_d88(false);
        bytes[D88_HEADER_SIZE + 8] = 0xb0;
        let image = MediaImage::parse(MediaFormat::D88, &bytes, false).unwrap();
        assert_eq!(image.sector_status(0, 0, 1, 0), Some(0xb0));
    }

    #[test]
    /// `dim_sparse_track_flags_map_compact_payload` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn dim_sparse_track_flags_map_compact_payload() {
        let mut bytes = vec![0; DIM_HEADER_SIZE + 2 * 9 * 1024];
        bytes[0] = 1;
        bytes[1] = 1;
        bytes[4] = 1;
        bytes[DIM_HEADER_SIZE + 9 * 1024] = 0x5a;
        let image = MediaImage::parse(MediaFormat::Dim, &bytes, true).unwrap();
        assert_eq!(image.read_sector(1, 1, 1, 3).unwrap()[0], 0x5a);
        assert!(image.read_sector(0, 1, 1, 3).is_none());
    }

    #[test]
    /// `malformed_media_inputs_never_panic` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn malformed_media_inputs_never_panic() {
        let mut seed = 0x1234_5678u32;
        for length in 0..1024 {
            let mut bytes = vec![0; length];
            for byte in &mut bytes {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *byte = seed as u8;
            }
            for format in [
                MediaFormat::Xdf,
                MediaFormat::Dim,
                MediaFormat::D88,
                MediaFormat::Hdf,
            ] {
                let _ = MediaImage::parse(format, &bytes, false);
            }
        }
    }
}
