//! X68000 CRTC のタイミング・スクロール・ラスタコピー状態。
//!
//! レジスタ配置と演算は PX68k `x68k/crtc.c` を比較資料としている。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    HorizontalSync,
    Raster,
    VerticalSync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Crtc {
    regs: Vec<u8>,
    mode: u8,
    width: u16,
    height: u16,
    h_start: u16,
    h_end: u16,
    v_start: u16,
    v_end: u16,
    v_total: u16,
    raster_line: u16,
    current_line: u16,
    text_scroll: [u16; 2],
    graphic_scroll: [[u16; 2]; 4],
    raster_copy_armed: [bool; 2],
}

impl Default for Crtc {
    fn default() -> Self {
        Self {
            regs: vec![0; 48],
            mode: 0,
            width: 768,
            height: 512,
            h_start: 28,
            h_end: 124,
            v_start: 40,
            v_end: 552,
            v_total: 568,
            raster_line: 0,
            current_line: 0,
            text_scroll: [0; 2],
            graphic_scroll: [[0; 2]; 4],
            raster_copy_armed: [false; 2],
        }
    }
}

impl Crtc {
    pub(crate) fn read(&self, offset: u32) -> u8 {
        if offset < 0x400 {
            let register = (offset & 0x3f) as usize;
            if (0x28..=0x2b).contains(&register) {
                return self.regs[register];
            }
        } else if offset == 0x481 {
            return self.mode;
        }
        0
    }

    /// レジスタを書き、GVRAM fast-clearが要求された場合は保持するbit maskを返す。
    pub(crate) fn write(&mut self, offset: u32, value: u8, tvram: &mut [u8]) -> Option<u16> {
        if offset < 0x400 {
            let register = (offset & 0x3f) as usize;
            if register >= self.regs.len() || self.regs[register] == value {
                return None;
            }
            self.regs[register] = value;
            self.recalculate(register);
            if register == 0x2c || register == 0x2d {
                self.raster_copy_armed[register - 0x2c] = true;
                if self.mode & 8 != 0 && self.raster_copy_armed[1] {
                    self.raster_copy(tvram);
                }
            }
        } else if offset == 0x481 {
            self.mode = value | (self.mode & 2);
            if self.mode & 8 != 0 {
                self.raster_copy(tvram);
            }
            if self.mode & 2 != 0 {
                const MASKS: [u16; 16] = [
                    0xffff, 0xfff0, 0xff0f, 0xff00, 0xf0ff, 0xf0f0, 0xf00f, 0xf000, 0x0fff, 0x0ff0,
                    0x0f0f, 0x0f00, 0x00ff, 0x00f0, 0x000f, 0x0000,
                ];
                self.mode &= !2;
                return Some(MASKS[usize::from(self.regs[0x2b] & 0x0f)]);
            }
        }
        None
    }

    pub(crate) fn next_scanline(&mut self) -> Vec<Signal> {
        self.current_line = (self.current_line + 1) % self.v_total.max(1);
        let mut signals = vec![Signal::HorizontalSync];
        if self.current_line == self.raster_line {
            signals.push(Signal::Raster);
        }
        if self.current_line == self.v_start || self.current_line == self.v_end {
            signals.push(Signal::VerticalSync);
        }
        signals
    }

    pub(crate) fn dimensions(&self) -> (u32, u32) {
        (u32::from(self.width), u32::from(self.height))
    }

    pub(crate) fn text_scroll(&self) -> (u16, u16) {
        (self.text_scroll[0], self.text_scroll[1])
    }

    pub(crate) fn graphic_scrolls(&self) -> [[u16; 2]; 4] {
        self.graphic_scroll
    }

    pub(crate) fn memory_mode(&self) -> u8 {
        self.regs[0x28]
    }

    /// CRTC R21の同時アクセス／マスク設定を通してテキストVRAMへ書く。
    pub(crate) fn write_tvram(&self, offset: u32, value: u8, tvram: &mut [u8]) {
        let offset = (offset as usize) & 0x7_ffff;
        let masked = self.regs[0x2a] & 2 != 0;
        let mut write = |address: usize| {
            let Some(byte) = tvram.get_mut(address) else {
                return;
            };
            *byte = if masked {
                let mask = self.regs[0x2e + (address & 1)];
                (*byte & mask) | (value & !mask)
            } else {
                value
            };
        };
        if self.regs[0x2a] & 1 != 0 {
            let offset = offset & 0x1_ffff;
            for plane in 0..4 {
                if self.regs[0x2b] & (0x10 << plane) != 0 {
                    write(offset + plane * 0x20_000);
                }
            }
        } else {
            write(offset);
        }
    }

    pub(crate) fn v_total(&self) -> u16 {
        self.v_total
    }

    pub(crate) fn high_resolution(&self) -> bool {
        self.regs[0x29] & 0x10 != 0
    }

    pub(crate) fn fast_clear_dimensions(&self) -> (u32, u32) {
        (
            if self.regs[0x29] & 3 != 0 { 512 } else { 256 },
            if self.regs[0x29] & 4 != 0 { 512 } else { 256 },
        )
    }

    pub(crate) fn gpip(&self, horizontal_sync_high: bool) -> u8 {
        let visible = (self.v_start..self.v_end).contains(&self.current_line);
        let mut value = 0x20 | if visible { 0x13 } else { 0x03 };
        if self.current_line != self.raster_line {
            value |= 0x40;
        }
        value | if horizontal_sync_high { 0x80 } else { 0 }
    }

    fn pair(&self, register: usize) -> u16 {
        (u16::from(self.regs[register]) << 8) | u16::from(self.regs[register + 1])
    }

    fn recalculate(&mut self, register: usize) {
        match register {
            0x04 | 0x05 => self.h_start = self.pair(0x04) & 1023,
            0x06 | 0x07 => self.h_end = self.pair(0x06) & 1023,
            0x08 | 0x09 => self.v_total = (self.pair(0x08) & 1023).max(1),
            0x0c | 0x0d => self.v_start = self.pair(0x0c) & 1023,
            0x0e | 0x0f => self.v_end = self.pair(0x0e) & 1023,
            0x12 | 0x13 => self.raster_line = self.pair(0x12) & 1023,
            0x14 | 0x15 => self.text_scroll[0] = self.pair(0x14) & 1023,
            0x16 | 0x17 => self.text_scroll[1] = self.pair(0x16) & 1023,
            0x18..=0x27 => {
                let pair = (register - 0x18) / 2;
                let plane = pair / 2;
                let axis = pair & 1;
                let base = 0x18 + pair * 2;
                self.graphic_scroll[plane][axis] =
                    self.pair(base) & if plane == 0 { 1023 } else { 511 };
            }
            _ => {}
        }
        self.width = self
            .h_end
            .saturating_sub(self.h_start)
            .saturating_mul(8)
            .clamp(1, 1024);
        let raw_height = self.v_end.saturating_sub(self.v_start).max(1);
        self.height = match self.regs[0x29] & 0x14 {
            0x10 => raw_height / 2,
            0x04 => raw_height.saturating_mul(2),
            _ => raw_height,
        }
        .clamp(1, 1024);
    }

    fn raster_copy(&mut self, tvram: &mut [u8]) {
        let source = usize::from(self.regs[0x2c]) << 9;
        let destination = usize::from(self.regs[0x2d]) << 9;
        const LINE_BYTES: usize = 512;
        let mut line = [0u8; LINE_BYTES];
        for plane in 0..4 {
            if self.regs[0x2b] & (1 << plane) == 0 {
                continue;
            }
            let plane_offset = plane * 0x20_000;
            let source = source + plane_offset;
            let destination = destination + plane_offset;
            if source + LINE_BYTES <= tvram.len() && destination + LINE_BYTES <= tvram.len() {
                line.copy_from_slice(&tvram[source..source + LINE_BYTES]);
                tvram[destination..destination + LINE_BYTES].copy_from_slice(&line);
            }
        }
        self.raster_copy_armed = [false; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_pairs_update_visible_size_and_scroll() {
        let mut crtc = Crtc::default();
        let mut tvram = vec![0; 0x80000];
        crtc.write(0x04, 0, &mut tvram);
        crtc.write(0x05, 10, &mut tvram);
        crtc.write(0x06, 0, &mut tvram);
        crtc.write(0x07, 90, &mut tvram);
        crtc.write(0x14, 1, &mut tvram);
        crtc.write(0x15, 2, &mut tvram);
        assert_eq!(crtc.dimensions().0, 640);
        assert_eq!(crtc.text_scroll().0, 258);
    }

    #[test]
    fn concurrent_masked_tvram_write_updates_selected_planes() {
        let mut crtc = Crtc::default();
        let mut tvram = vec![0xa5; 0x80000];
        crtc.regs[0x2a] = 3;
        crtc.regs[0x2b] = 0x50; // planes 0 and 2
        crtc.regs[0x2e] = 0xf0;
        crtc.regs[0x2f] = 0x0f;
        crtc.write_tvram(1, 0x3c, &mut tvram);
        assert_eq!(tvram[1], 0x3c & 0xf0 | 0xa5 & 0x0f);
        assert_eq!(tvram[0x20001], 0xa5);
        assert_eq!(tvram[0x40001], 0x3c & 0xf0 | 0xa5 & 0x0f);
    }

    #[test]
    fn operation_port_returns_fast_clear_plane_mask() {
        let mut crtc = Crtc::default();
        let mut tvram = vec![0; 0x80000];
        crtc.write(0x2b, 0x05, &mut tvram);
        assert_eq!(crtc.write(0x481, 2, &mut tvram), Some(0xf0f0));
        assert_eq!(crtc.read(0x481) & 2, 0);
    }
}
