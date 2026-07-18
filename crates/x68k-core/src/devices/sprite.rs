//! スプライト/BG レジスタ、パターンRAM、画素生成。
//!
//! 配置と制御ビットは PX68k `x68k/bg.c` を比較資料としている。

use serde::{Deserialize, Serialize};

use super::video::Video;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SpriteBg {
    bytes: Vec<u8>,
}

impl Default for SpriteBg {
    fn default() -> Self {
        Self {
            bytes: vec![0; 0x1_0000],
        }
    }
}

impl SpriteBg {
    pub(crate) fn read(&self, offset: u32) -> u8 {
        self.bytes.get(offset as usize).copied().unwrap_or(0xff)
    }

    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        if let Some(byte) = self.bytes.get_mut(offset as usize) {
            *byte = value;
        }
    }

    pub(crate) fn pixel(&self, video: &Video, x: u32, y: u32) -> Option<u16> {
        if self.bytes[0x808] & 2 == 0 {
            return None;
        }
        // 実機の内部順序: sprite pri1 < BG1 < sprite pri2 < BG0 < sprite pri3。
        let mut result = self.sprite_pixel(video, x, y, 1);
        if self.bytes[0x809] & 8 != 0 && self.bytes[0x811] & 3 == 0 {
            result = self.bg_pixel(video, x, y, 1).or(result);
        }
        result = self.sprite_pixel(video, x, y, 2).or(result);
        if self.bytes[0x809] & 1 != 0 {
            result = self.bg_pixel(video, x, y, 0).or(result);
        }
        self.sprite_pixel(video, x, y, 3).or(result)
    }

    fn sprite_pixel(&self, video: &Video, x: u32, y: u32, priority: u8) -> Option<u16> {
        let mut result = None;
        for number in (0..128).rev() {
            let base = number * 8;
            let px = u16::from_be_bytes([self.bytes[base], self.bytes[base + 1]]) as i32 - 16;
            let py = u16::from_be_bytes([self.bytes[base + 2], self.bytes[base + 3]]) as i32 - 16;
            let local_x = x as i32 - px;
            let local_y = y as i32 - py;
            if !(0..16).contains(&local_x) || !(0..16).contains(&local_y) {
                continue;
            }
            let control = u16::from_be_bytes([self.bytes[base + 4], self.bytes[base + 5]]);
            if self.bytes[base + 6] & 3 != priority {
                continue;
            }
            let sx = if control & 0x4000 != 0 {
                15 - local_x
            } else {
                local_x
            } as usize;
            let sy = if control & 0x8000 != 0 {
                15 - local_y
            } else {
                local_y
            } as usize;
            let pattern = usize::from(control & 0x00ff);
            let address = 0x8000 + pattern_address(pattern, 16, sx, sy);
            let packed = self.bytes.get(address).copied().unwrap_or(0);
            let nibble = if sx & 1 == 0 {
                packed >> 4
            } else {
                packed & 0x0f
            };
            if nibble != 0 {
                let palette = ((control >> 4) as u8 & 0xf0) | nibble;
                result = Some(video.text_colour(palette));
            }
        }
        result
    }

    fn bg_pixel(&self, video: &Video, x: u32, y: u32, plane: usize) -> Option<u16> {
        let control = self.bytes[0x811];
        let tile_size = if control & 3 == 0 { 8usize } else { 16 };
        let mask = if tile_size == 8 { 511u32 } else { 1023 };
        let scroll_base = plane * 4;
        let scroll_x = u16::from_be_bytes([
            self.bytes[0x800 + scroll_base],
            self.bytes[0x801 + scroll_base],
        ]) as u32;
        let scroll_y = u16::from_be_bytes([
            self.bytes[0x802 + scroll_base],
            self.bytes[0x803 + scroll_base],
        ]) as u32;
        let sx = (x + scroll_x) & mask;
        let sy = (y + scroll_y) & mask;
        let plane_config = self.bytes[0x809];
        let alternate_map = if plane == 0 {
            plane_config & 0x06 != 0
        } else {
            plane_config & 0x30 != 0
        };
        let map_base = if alternate_map { 0xe000 } else { 0xc000 };
        let tiles_per_row = 64;
        let entry =
            map_base + ((sy as usize / tile_size) * tiles_per_row + sx as usize / tile_size) * 2;
        let descriptor = u16::from_be_bytes([
            self.bytes.get(entry).copied().unwrap_or(0),
            self.bytes.get(entry + 1).copied().unwrap_or(0),
        ]);
        let mut local_x = sx as usize % tile_size;
        let mut local_y = sy as usize % tile_size;
        if descriptor & 0x4000 != 0 {
            local_x = tile_size - 1 - local_x;
        }
        if descriptor & 0x8000 != 0 {
            local_y = tile_size - 1 - local_y;
        }
        let pattern = usize::from(descriptor & 0x00ff);
        let address = 0x8000 + pattern_address(pattern, tile_size, local_x, local_y);
        let packed = self.bytes.get(address).copied().unwrap_or(0);
        let nibble = if local_x & 1 == 0 {
            packed >> 4
        } else {
            packed & 0x0f
        };
        (nibble != 0).then(|| {
            let palette = ((descriptor >> 4) as u8 & 0xf0) | nibble;
            video.text_colour(palette)
        })
    }
}

fn pattern_address(pattern: usize, tile_size: usize, x: usize, y: usize) -> usize {
    if tile_size == 8 {
        pattern * 32 + y * 4 + x / 2
    } else {
        pattern * 128 + y * 4 + (x / 2 & 3) + usize::from(x >= 8) * 0x40
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_pattern_uses_text_palette() {
        let mut video = Video::default();
        video.write(0x200 + 2, 0x07);
        video.write(0x200 + 3, 0xc0);
        let mut sprites = SpriteBg::default();
        sprites.write(0, 0);
        sprites.write(1, 16);
        sprites.write(2, 0);
        sprites.write(3, 16);
        sprites.write(6, 1);
        sprites.write(0x8000, 0x10);
        sprites.write(0x808, 2);
        assert_eq!(sprites.pixel(&video, 0, 0), Some(0x07c0));
    }

    #[test]
    fn bg_planes_and_sprite_priorities_follow_hardware_order() {
        let mut video = Video::default();
        for (index, colour) in [(1usize, 0x1111u16), (2, 0x2222), (3, 0x3333)] {
            let offset = 0x200 + index * 2;
            let [high, low] = colour.to_be_bytes();
            video.write(offset as u32, high);
            video.write(offset as u32 + 1, low);
        }
        let mut ram = SpriteBg::default();
        ram.write(0x808, 2);
        ram.write(0x809, 0x19); // BG0 and BG1, BG1 map at 0x6000
        ram.write(0xc000, 0);
        ram.write(0xc001, 2);
        ram.write(0xe000, 0);
        ram.write(0xe001, 1);
        ram.write(0x8020, 0x10);
        ram.write(0x8040, 0x20);
        assert_eq!(ram.pixel(&video, 0, 0), Some(0x2222), "BG0 overlays BG1");

        ram.write(0, 0);
        ram.write(1, 16);
        ram.write(2, 0);
        ram.write(3, 16);
        ram.write(4, 0);
        ram.write(5, 3);
        ram.write(6, 3);
        ram.write(0x8180, 0x30);
        assert_eq!(
            ram.pixel(&video, 0, 0),
            Some(0x3333),
            "priority 3 sprite is top"
        );
    }
}
