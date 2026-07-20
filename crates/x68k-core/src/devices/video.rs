//! パレット、色モード、グラフィックプレーン合成を行うビデオ制御回路。

use serde::{Deserialize, Serialize};

use super::gvram::GraphicVram;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Video {
    palette_bytes: Vec<u8>,
    registers: [u8; 6],
}

impl Default for Video {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            palette_bytes: vec![0; 1024],
            registers: [0; 6],
        }
    }
}

impl Video {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&self, offset: u32) -> u8 {
        match offset {
            0..=0x3ff => self.palette_bytes[offset as usize],
            0x400..=0x401 => self.registers[(offset - 0x400) as usize],
            0x500..=0x501 => self.registers[2 + (offset - 0x500) as usize],
            0x600..=0x601 => self.registers[4 + (offset - 0x600) as usize],
            _ => 0xff,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        match offset {
            0..=0x3ff => self.palette_bytes[offset as usize] = value,
            0x400..=0x401 => self.registers[(offset - 0x400) as usize] = value,
            0x500..=0x501 => self.registers[2 + (offset - 0x500) as usize] = value,
            0x600..=0x601 => self.registers[4 + (offset - 0x600) as usize] = value,
            _ => {}
        }
    }

    /// グラフィックVRAM層が現在表示許可されているかを返す。
    pub(crate) fn graphics_enabled(&self) -> bool {
        self.registers[5] & 0x1f != 0
    }

    /// テキストVRAM層が現在表示許可されているかを返す。
    pub(crate) fn text_enabled(&self) -> bool {
        self.registers[5] & 0x20 != 0
    }

    /// スプライト/BG層が現在表示許可されているかを返す。
    pub(crate) fn sprites_enabled(&self) -> bool {
        self.registers[5] & 0x40 != 0
    }

    #[cfg(test)]
    /// 現在のレジスタ値または入力から `graphic_pixel` に対応する描画・転送情報を算出して返す。
    fn graphic_pixel(
        &self,
        vram: &GraphicVram,
        scroll: [[u16; 2]; 4],
        x: u32,
        y: u32,
    ) -> Option<u16> {
        self.graphic_pixel_with_attributes(vram, scroll, x, y)
            .map(|sample| sample.0)
    }

    /// グラフィックVRAMの画素値と特殊合成属性を現在の色モードに従って取得する。
    pub(crate) fn graphic_pixel_with_attributes(
        &self,
        vram: &GraphicVram,
        scroll: [[u16; 2]; 4],
        x: u32,
        y: u32,
    ) -> Option<(u16, bool)> {
        match self.registers[1] & 3 {
            0 => self.pixel_16(vram, scroll, x, y),
            1 | 2 => self.pixel_256(vram, scroll, x, y),
            _ => self.pixel_65536(vram, scroll, x, y),
        }
    }

    /// 現在のレジスタ値または入力から `text_colour` に対応する描画・転送情報を算出して返す。
    pub(crate) fn text_colour(&self, index: u8) -> u16 {
        self.palette(256 + usize::from(index))
    }

    /// 全表示layerが透明な画素へ出るgraphics palette 0（backdrop）。
    pub(crate) fn backdrop_colour(&self) -> u16 {
        self.palette(0)
    }

    /// 現在のレジスタ値または入力から `layer_priority` に対応する描画・転送情報を算出して返す。
    pub(crate) fn layer_priority(&self, layer: usize) -> u8 {
        let priority = match layer {
            0 => self.registers[2] & 3,
            1 => (self.registers[2] >> 2) & 3,
            _ => (self.registers[2] >> 4) & 3,
        };
        priority.min(2)
    }

    /// 半透明特殊合成が現在有効かを返す。
    pub(crate) fn half_transparency_enabled(&self) -> bool {
        self.registers[4] & 0x5d == 0x1c
    }

    /// 特殊優先順位合成が現在有効かを返す。
    pub(crate) fn special_priority_enabled(&self) -> bool {
        self.registers[4] & 0x5c == 0x14
    }

    /// `special_pixel` の条件が現在成立しているかを、副作用なく判定して返す。
    fn special_pixel(&self, index: u16) -> bool {
        (self.half_transparency_enabled() || self.special_priority_enabled()) && index & 1 != 0
    }

    /// 16色モードのプレーン優先順位に従いグラフィック画素を合成する。
    fn pixel_16(
        &self,
        vram: &GraphicVram,
        scroll: [[u16; 2]; 4],
        x: u32,
        y: u32,
    ) -> Option<(u16, bool)> {
        let mut colour = 0;
        for rank in (0..4).rev() {
            if self.registers[5] & (1 << rank) == 0 {
                continue;
            }
            let plane = ((self.registers[3] >> (rank * 2)) & 3) as usize;
            let px = x + u32::from(scroll[plane][0]);
            let py = y + u32::from(scroll[plane][1]);
            let (low, high) = vram.word(px, py);
            let index = match plane {
                0 => low & 0x0f,
                1 => low >> 4,
                2 => high & 0x0f,
                _ => high >> 4,
            };
            if index != 0 {
                colour = index;
            }
        }
        (colour != 0).then(|| {
            let special = self.special_pixel(u16::from(colour));
            let index = if special { colour & !1 } else { colour };
            (self.palette(usize::from(index)), special)
        })
    }

    /// 256色モードのプレーン組合せからパレット番号を復元する。
    fn pixel_256(
        &self,
        vram: &GraphicVram,
        scroll: [[u16; 2]; 4],
        x: u32,
        y: u32,
    ) -> Option<(u16, bool)> {
        let mut colour = 0;
        let page0_priority = self.registers[3] & 3;
        let page1_priority = (self.registers[3] >> 4) & 3;
        let order = if page0_priority <= page1_priority {
            [1usize, 0]
        } else {
            [0usize, 1]
        };
        for page in order {
            let enable = if page == 0 { 1 } else { 4 };
            if self.registers[5] & enable == 0 {
                continue;
            }
            let first = vram.word(
                x + u32::from(scroll[page * 2][0]),
                y + u32::from(scroll[page * 2][1]),
            );
            let second = vram.word(
                x + u32::from(scroll[page * 2 + 1][0]),
                y + u32::from(scroll[page * 2 + 1][1]),
            );
            let (low, high) = if page == 0 {
                (first.0, second.0)
            } else {
                (first.1, second.1)
            };
            let index = (low & 0x0f) | (high & 0xf0);
            if index != 0 {
                colour = index;
            }
        }
        (colour != 0).then(|| {
            let special = self.special_pixel(u16::from(colour));
            let index = if special { colour & !1 } else { colour };
            (self.palette(usize::from(index)), special)
        })
    }

    /// 65536色モードの4プレーンから16bit GRBi画素を復元する。
    fn pixel_65536(
        &self,
        vram: &GraphicVram,
        scroll: [[u16; 2]; 4],
        x: u32,
        y: u32,
    ) -> Option<(u16, bool)> {
        if self.registers[5] & 0x0f == 0 {
            return None;
        }
        let (low, high) = vram.word(x + u32::from(scroll[0][0]), y + u32::from(scroll[0][1]));
        let colour = u16::from_be_bytes([high, low]);
        (colour != 0).then(|| {
            let special = self.special_pixel(colour);
            (if special { colour & !1 } else { colour }, special)
        })
    }

    /// 現在のレジスタ値または入力から `palette` に対応する描画・転送情報を算出して返す。
    fn palette(&self, index: usize) -> u16 {
        let offset = index * 2;
        u16::from_be_bytes([self.palette_bytes[offset], self.palette_bytes[offset + 1]])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `palette_and_direct_colour_modes_emit_grbi` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn palette_and_direct_colour_modes_emit_grbi() {
        let mut video = Video::default();
        let mut vram = GraphicVram::default();
        video.write(2, 0x07);
        video.write(3, 0xc0);
        video.write(0x401, 0);
        video.write(0x501, 0);
        video.write(0x601, 1);
        vram.write(1, 1, 0);
        assert_eq!(video.graphic_pixel(&vram, [[0; 2]; 4], 0, 0), Some(0x07c0));
    }

    #[test]
    /// `graphics_palette_zero_is_the_backdrop_colour` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn graphics_palette_zero_is_the_backdrop_colour() {
        let mut video = Video::default();
        video.write(0, 0x68);
        video.write(1, 0x4e);
        assert_eq!(video.backdrop_colour(), 0x684e);
    }

    #[test]
    /// `mode_256_combines_nibbles_from_each_page_byte` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn mode_256_combines_nibbles_from_each_page_byte() {
        let mut video = Video::default();
        let mut vram = GraphicVram::default();
        video.write(0x401, 1); // 256 colours
        video.write(0x501, 0x20); // GRP0 above GRP1
        video.write(0x601, 0x05); // both 256-colour pages enabled
        video.write(0x14a, 0x12); // palette 0xa5
        video.write(0x14b, 0x34);
        vram.write(1, 0xa5, 1);
        vram.write(0x80001, 0x3c, 1);
        assert_eq!(video.graphic_pixel(&vram, [[0; 2]; 4], 0, 0), Some(0x1234));
    }

    #[test]
    /// `layer_priority_three_is_same_as_lowest_priority_two` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn layer_priority_three_is_same_as_lowest_priority_two() {
        let mut video = Video::default();
        video.write(0x500, 0x3b);
        assert_eq!(video.layer_priority(0), 2);
        assert_eq!(video.layer_priority(1), 2);
        assert_eq!(video.layer_priority(2), 2);
    }

    #[test]
    /// `special_modes_only_mark_pixels_with_low_bit_set` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn special_modes_only_mark_pixels_with_low_bit_set() {
        let mut video = Video::default();
        let mut vram = GraphicVram::default();
        video.write(0x401, 3);
        video.write(0x601, 1);
        video.write(0x600, 0x1c);
        vram.write(0, 0x12, 8);
        vram.write(1, 0x35, 8);
        assert_eq!(
            video.graphic_pixel_with_attributes(&vram, [[0; 2]; 4], 0, 0),
            Some((0x1234, true))
        );
        video.write(0x600, 0);
        assert_eq!(
            video.graphic_pixel_with_attributes(&vram, [[0; 2]; 4], 0, 0),
            Some((0x1235, false))
        );
    }
}
