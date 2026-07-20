//! X68000 GVRAM のCPUウィンドウと物理512KiBの変換。
//!
//! ページ/ニブル配置は PX68k `x68k/gvram.c` を比較資料としている。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GraphicVram {
    bytes: Vec<u8>,
}

impl Default for GraphicVram {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            bytes: vec![0; 0x80000],
        }
    }
}

impl GraphicVram {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&self, address: u32, memory_mode: u8) -> u8 {
        let address = address & 0x1f_ffff;
        match mode(memory_mode) {
            0 => {
                if address & 1 == 0 {
                    return 0;
                }
                let index = (((address >> 1) & 0x7fc00) | (address & 0x3ff)) as usize;
                let byte = self
                    .bytes
                    .get(index ^ usize::from(address & 0x10_0000 == 0))
                    .copied()
                    .unwrap_or(0);
                if address & 0x400 == 0 {
                    byte & 0x0f
                } else {
                    byte >> 4
                }
            }
            1 => {
                if address & 1 == 0 {
                    return 0;
                }
                let index = (address & 0x7ffff) as usize;
                let byte = self
                    .bytes
                    .get(index ^ usize::from(address < 0x10_0000))
                    .copied()
                    .unwrap_or(0);
                if address & 0x08_0000 == 0 {
                    byte & 0x0f
                } else {
                    byte >> 4
                }
            }
            2 | 3 => {
                if address & 1 == 0 || address >= 0x10_0000 {
                    return 0;
                }
                let index = (address & 0x7ffff) as usize;
                self.bytes
                    .get(index ^ usize::from(address < 0x8_0000))
                    .copied()
                    .unwrap_or(0)
            }
            _ => {
                if address >= 0x8_0000 {
                    0
                } else {
                    self.bytes[(address as usize) ^ 1]
                }
            }
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, address: u32, value: u8, memory_mode: u8) {
        let address = address & 0x1f_ffff;
        match mode(memory_mode) {
            0 => {
                if address & 1 == 0 {
                    return;
                }
                let index = (((address & 0xff800) >> 1) + (address & 0x3ff)) as usize;
                let index = index ^ usize::from(address & 0x10_0000 == 0);
                let high = address & 0x400 != 0;
                set_nibble(&mut self.bytes, index, value, high);
            }
            1 => {
                if address & 1 == 0 {
                    return;
                }
                let page = address >> 19;
                let index = (address & 0x7ffff) as usize ^ usize::from(page < 2);
                set_nibble(&mut self.bytes, index, value, page & 1 != 0);
            }
            2 | 3 => {
                if address & 1 != 0 && address < 0x10_0000 {
                    let index = (address & 0x7ffff) as usize ^ usize::from(address < 0x8_0000);
                    if let Some(byte) = self.bytes.get_mut(index) {
                        *byte = value;
                    }
                }
            }
            _ => {
                if address < 0x8_0000 {
                    self.bytes[(address as usize) ^ 1] = value;
                }
            }
        }
    }

    /// 入力値を `word` に対応する内部表現へ変換して返す。
    pub(crate) fn word(&self, x: u32, y: u32) -> (u8, u8) {
        let offset = (((y & 511) * 512 + (x & 511)) * 2) as usize;
        (self.bytes[offset], self.bytes[offset + 1])
    }

    /// CRTC高速クリア領域のVRAMをハードウェア指定値で消去する。
    pub(crate) fn fast_clear(
        &mut self,
        retain_mask: u16,
        scroll: [u16; 2],
        width: u32,
        height: u32,
    ) {
        for y in 0..height.min(512) {
            for x in 0..width.min(512) {
                let offset = ((((y + u32::from(scroll[1])) & 511) * 512
                    + ((x + u32::from(scroll[0])) & 511))
                    * 2) as usize;
                let value =
                    u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]]) & retain_mask;
                self.bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
    }
}

/// 現在のレジスタ値または入力から `mode` に対応する描画・転送情報を算出して返す。
fn mode(memory_mode: u8) -> u8 {
    if memory_mode & 8 != 0 {
        4
    } else if memory_mode & 4 != 0 {
        0
    } else {
        (memory_mode & 3) + 1
    }
}

/// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
fn set_nibble(bytes: &mut [u8], index: usize, value: u8, high: bool) {
    if let Some(byte) = bytes.get_mut(index) {
        *byte = if high {
            (*byte & 0x0f) | (value << 4)
        } else {
            (*byte & 0xf0) | (value & 0x0f)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `cpu_window_round_trips_all_colour_modes` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn cpu_window_round_trips_all_colour_modes() {
        for memory_mode in [4, 0, 1, 8] {
            let mut vram = GraphicVram::default();
            vram.write(1, 0x0a, memory_mode);
            assert_eq!(
                vram.read(1, memory_mode),
                if memory_mode == 8 { 0x0a } else { 0x0a }
            );
        }
    }

    #[test]
    /// `fast_clear_obeys_scroll_wrap_and_plane_mask` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn fast_clear_obeys_scroll_wrap_and_plane_mask() {
        let mut vram = GraphicVram {
            bytes: vec![0xff; 0x80000],
        };
        vram.fast_clear(0xf0f0, [511, 511], 2, 2);
        for (x, y) in [(511, 511), (0, 511), (511, 0), (0, 0)] {
            let (low, high) = vram.word(x, y);
            assert_eq!([low, high], [0xf0, 0xf0]);
        }
        assert_eq!(vram.word(1, 1), (0xff, 0xff));
    }
}
