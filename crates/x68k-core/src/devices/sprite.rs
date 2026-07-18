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

    pub(crate) fn diagnostics(&self) -> Vec<(u8, u16, u16, u16, u8)> {
        (0..128)
            .filter_map(|number| {
                let base = number * 8;
                let priority = self.bytes[base + 7] & 3;
                (priority != 0).then(|| {
                    (
                        number as u8,
                        u16::from_be_bytes([self.bytes[base], self.bytes[base + 1]]) & 0x03ff,
                        u16::from_be_bytes([self.bytes[base + 2], self.bytes[base + 3]]) & 0x03ff,
                        u16::from_be_bytes([self.bytes[base + 4], self.bytes[base + 5]]),
                        priority,
                    )
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn pixel(&self, video: &Video, x: u32, y: u32) -> Option<u16> {
        if self.bytes[0x808] & 2 == 0 {
            return None;
        }
        // 実機の内部順序: sprite pri1 < BG1 < sprite pri2 < BG0 < sprite pri3。
        let mut result = self.sprite_pixel(video, x, y, 1);
        if self.bytes[0x809] & 8 != 0 {
            result = self.bg_pixel(video, x, y, 1).or(result);
        }
        result = self.sprite_pixel(video, x, y, 2).or(result);
        if self.bytes[0x809] & 1 != 0 {
            result = self.bg_pixel(video, x, y, 0).or(result);
        }
        self.sprite_pixel(video, x, y, 3).or(result)
    }

    /// Sprite/BG内部の優先順で1frameを合成する。bit 16を有効flag、下位16bitを
    /// GRBiとして返し、paletteが黒の不透明pixelも透明色と区別する。
    #[cfg(test)]
    pub(crate) fn render(&self, video: &Video, width: u32, height: u32) -> Vec<u32> {
        let mut frame = vec![0; (width * height) as usize];
        for y in 0..height {
            self.render_scanline(
                video,
                width,
                y,
                &mut frame[(y * width) as usize..][..width as usize],
            );
        }
        frame
    }

    /// 現在のsprite RAMを用いて1走査線だけを合成する。XSP等がraster IRQで
    /// sprite番号を再利用するため、実フレームの最終状態から全行を描いてはならない。
    pub(crate) fn render_scanline(&self, video: &Video, width: u32, y: u32, line: &mut [u32]) {
        line.fill(0);
        if self.bytes[0x808] & 2 == 0 {
            return;
        }
        let (selected, selected_count) = self.selected_sprites(y);
        self.draw_sprites_scanline(video, width, y, 1, &selected[..selected_count], line);
        if self.bytes[0x809] & 8 != 0 {
            self.draw_bg_scanline(video, width, y, 1, line);
        }
        self.draw_sprites_scanline(video, width, y, 2, &selected[..selected_count], line);
        if self.bytes[0x809] & 1 != 0 {
            self.draw_bg_scanline(video, width, y, 0, line);
        }
        self.draw_sprites_scanline(video, width, y, 3, &selected[..selected_count], line);
    }

    /// Cynthiaが1走査線で選択できるのはsprite番号の小さい方から32枚。
    /// priorityはBGとの前後関係であり、この選択順やsprite間の前後関係を変えない。
    fn selected_sprites(&self, y: u32) -> ([u8; 32], usize) {
        let mut selected = [0; 32];
        let mut count = 0;
        for number in 0..128 {
            let base = number * 8;
            if self.bytes[base + 7] & 3 == 0 {
                continue;
            }
            let py = (u16::from_be_bytes([self.bytes[base + 2], self.bytes[base + 3]]) & 0x03ff)
                as i32
                - 16;
            if (py..py + 16).contains(&(y as i32)) {
                selected[count] = number as u8;
                count += 1;
                if count == selected.len() {
                    break;
                }
            }
        }
        (selected, count)
    }

    fn draw_sprites_scanline(
        &self,
        video: &Video,
        width: u32,
        y: u32,
        priority: u8,
        selected: &[u8],
        line: &mut [u32],
    ) {
        // 大きいsprite番号から描き、同一priorityでは小さい番号を前面にする。
        for &number in selected.iter().rev() {
            let number = usize::from(number);
            let base = number * 8;
            if self.bytes[base + 7] & 3 != priority {
                continue;
            }
            let px =
                (u16::from_be_bytes([self.bytes[base], self.bytes[base + 1]]) & 0x03ff) as i32 - 16;
            let py = (u16::from_be_bytes([self.bytes[base + 2], self.bytes[base + 3]]) & 0x03ff)
                as i32
                - 16;
            let control = u16::from_be_bytes([self.bytes[base + 4], self.bytes[base + 5]]);
            let pattern = usize::from(control & 0x00ff);
            let local_y = y as i32 - py;
            let sy = if control & 0x8000 != 0 {
                15 - local_y
            } else {
                local_y
            } as usize;
            for local_x in 0..16 {
                let screen_x = px + local_x;
                if !(0..width as i32).contains(&screen_x) {
                    continue;
                }
                let sx = if control & 0x4000 != 0 {
                    15 - local_x
                } else {
                    local_x
                } as usize;
                let address = 0x8000 + pattern_address(pattern, 16, sx, sy);
                let packed = self.bytes.get(address).copied().unwrap_or(0);
                let nibble = if sx & 1 == 0 {
                    packed >> 4
                } else {
                    packed & 0x0f
                };
                if nibble != 0 {
                    let palette = ((control >> 4) as u8 & 0xf0) | nibble;
                    line[screen_x as usize] = 0x1_0000 | u32::from(video.text_colour(palette));
                }
            }
        }
    }

    fn draw_bg_scanline(&self, video: &Video, width: u32, y: u32, plane: usize, line: &mut [u32]) {
        for x in 0..width {
            if let Some(colour) = self.bg_pixel(video, x, y, plane) {
                line[x as usize] = 0x1_0000 | u32::from(colour);
            }
        }
    }

    #[cfg(test)]
    fn sprite_pixel(&self, video: &Video, x: u32, y: u32, priority: u8) -> Option<u16> {
        let mut result = None;
        for number in (0..128).rev() {
            let base = number * 8;
            // 座標レジスタは10bit。上位の未使用bitは描画座標へ混ぜない。
            let px =
                (u16::from_be_bytes([self.bytes[base], self.bytes[base + 1]]) & 0x03ff) as i32 - 16;
            let py = (u16::from_be_bytes([self.bytes[base + 2], self.bytes[base + 3]]) & 0x03ff)
                as i32
                - 16;
            let local_x = x as i32 - px;
            let local_y = y as i32 - py;
            if !(0..16).contains(&local_x) || !(0..16).contains(&local_y) {
                continue;
            }
            let control = u16::from_be_bytes([self.bytes[base + 4], self.bytes[base + 5]]);
            // +6は16bit優先度レジスタで、priorityはbit 1-0、つまり
            // big-endianバス上の下位byte（+7）にある。上位byteを読むと
            // 実ソフトのmove.w #priority,$eb0006を常にpriority 0としてしまう。
            if self.bytes[base + 7] & 3 != priority {
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
        let bg0_map_selected = if plane == 0 {
            plane_config & 0x06 == 0x02
        } else {
            plane_config & 0x30 == 0x10
        };
        let map_base = if bg0_map_selected { 0xe000 } else { 0xc000 };
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
        sprites.write(7, 1);
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
        ram.write(7, 3);
        ram.write(0x8180, 0x30);
        assert_eq!(
            ram.pixel(&video, 0, 0),
            Some(0x3333),
            "priority 3 sprite is top"
        );
    }

    #[test]
    fn sprite_priority_uses_low_byte_of_the_word_register() {
        let mut video = Video::default();
        video.write(0x202, 0x07);
        video.write(0x203, 0xc0);
        let mut sprites = SpriteBg::default();
        sprites.write(1, 16);
        sprites.write(3, 16);
        sprites.write(0x8000, 0x10);
        sprites.write(0x808, 2);

        sprites.write(6, 3);
        assert_eq!(sprites.pixel(&video, 0, 0), None);
        sprites.write(6, 0);
        sprites.write(7, 3);
        assert_eq!(sprites.pixel(&video, 0, 0), Some(0x07c0));
    }

    #[test]
    fn sprite_coordinates_are_ten_bit_registers() {
        let mut video = Video::default();
        video.write(0x202, 0x07);
        video.write(0x203, 0xc0);
        let mut sprites = SpriteBg::default();
        sprites.write(0, 0xfc);
        sprites.write(1, 16);
        sprites.write(2, 0xfc);
        sprites.write(3, 16);
        sprites.write(7, 3);
        sprites.write(0x8000, 0x10);
        sprites.write(0x808, 2);
        assert_eq!(sprites.pixel(&video, 0, 0), Some(0x07c0));
    }

    #[test]
    fn scanline_selects_only_the_first_32_sprite_numbers() {
        let mut video = Video::default();
        video.write(0x202, 0x07);
        video.write(0x203, 0xc0);
        let mut sprites = SpriteBg::default();
        sprites.write(0x808, 2);
        sprites.write(0x8080, 0x10); // pattern 1だけを不透明にする
        for number in 0..33 {
            let base = number * 8;
            sprites.write(base + 1, 16);
            sprites.write(base + 3, 16);
            sprites.write(base + 5, u8::from(number == 32));
            sprites.write(base + 7, 3);
        }

        let mut line = [0u32; 1];
        sprites.render_scanline(&video, 1, 0, &mut line);
        assert_eq!(line[0], 0, "sprite 32 is outside the horizontal limit");

        sprites.write(7, 0);
        sprites.render_scanline(&video, 1, 0, &mut line);
        assert_eq!(line[0], 0x1_07c0, "a free slot admits sprite 32");
    }

    #[test]
    fn frame_renderer_matches_pixel_renderer_and_keeps_opaque_black() {
        let mut video = Video::default();
        video.write(0x202, 0x07);
        video.write(0x203, 0xc0);
        let mut sprites = SpriteBg::default();
        sprites.write(1, 16);
        sprites.write(3, 16);
        sprites.write(7, 3);
        sprites.write(0x8000, 0x12);
        sprites.write(0x808, 2);
        let frame = sprites.render(&video, 2, 1);
        for x in 0..2 {
            assert_eq!(
                (frame[x] & 0x1_0000 != 0).then_some(frame[x] as u16),
                sprites.pixel(&video, x as u32, 0)
            );
        }

        video.write(0x202, 0);
        video.write(0x203, 0);
        let frame = sprites.render(&video, 1, 1);
        assert_eq!(frame[0], 0x1_0000, "palette black remains opaque");
    }

    #[test]
    fn bg1_remains_visible_in_16_pixel_mode_and_txsel_is_exact() {
        let mut video = Video::default();
        video.write(0x202, 0x11);
        video.write(0x203, 0x11);
        video.write(0x204, 0x22);
        video.write(0x205, 0x22);
        let mut sprites = SpriteBg::default();
        sprites.write(0x808, 2);
        sprites.write(0x809, 0x08); // BG1 enable, BG1 map selected
        sprites.write(0x811, 1); // 16x16 mode
        sprites.write(0xc001, 1);
        sprites.write(0x8080, 0x10);
        assert_eq!(sprites.render(&video, 1, 1)[0], 0x1_1111);

        sprites.write(0x809, 0x18); // BG1 TXSEL=01: BG0 map selected
        sprites.write(0xe001, 2);
        sprites.write(0x8100, 0x20);
        assert_eq!(sprites.render(&video, 1, 1)[0], 0x1_2222);

        sprites.write(0x809, 0x28); // TXSEL=10 is not the BG0 map
        assert_eq!(sprites.render(&video, 1, 1)[0], 0x1_1111);
    }
}
