//! MC68901 MFP の割り込み、タイマ、キーボード USART。
//!
//! 初期値・優先順位・レジスタ副作用は PX68k `x68k/mfp.c` を比較資料としている。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

const IERA: usize = 3;
const IERB: usize = 4;
const IPRA: usize = 5;
const IPRB: usize = 6;
const ISRA: usize = 7;
const ISRB: usize = 8;
const IMRA: usize = 9;
const IMRB: usize = 10;
const VR: usize = 11;
const TACR: usize = 12;
const TBCR: usize = 13;
const TCDCR: usize = 14;
const TADR: usize = 15;
const RSR: usize = 21;
const TSR: usize = 22;
const UDR: usize = 23;
const TDDR: usize = TADR + 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Mfp {
    regs: [u8; 24],
    timer_reload: [u8; 4],
    timer_ticks: [u32; 4],
    clock_fraction: u64,
    serial: VecDeque<u8>,
}

impl Default for Mfp {
    fn default() -> Self {
        Self {
            regs: [
                0x7b, 0x06, 0x00, 0x18, 0x3e, 0x00, 0x00, 0x00, 0x00, 0x18, 0x3e, 0x40, 0x08, 0x01,
                0x77, 0x01, 0x0d, 0xc8, 0x14, 0x00, 0x88, 0x01, 0x81, 0x00,
            ],
            timer_reload: [1, 0x0d, 0xc8, 0x14],
            timer_ticks: [0; 4],
            clock_fraction: 0,
            serial: VecDeque::new(),
        }
    }
}

impl Mfp {
    pub(crate) fn read(&mut self, offset: u32, gpip: u8) -> u8 {
        if offset > 0x2f || offset & 1 == 0 {
            return 0xff;
        }
        let register = (offset as usize & 0x3f) >> 1;
        match register {
            0 => gpip,
            RSR => {
                if self.serial.is_empty() {
                    self.regs[RSR] | 0x80
                } else {
                    self.regs[RSR] & 0x7f
                }
            }
            UDR => {
                let value = self.serial.pop_front().unwrap_or(0);
                // 受信FIFOに次のキーが残る場合、現在の割り込みがackされた後にも
                // 読み出せるよう受信full要求を再度立てる。
                if !self.serial.is_empty() {
                    self.raise(3);
                }
                value
            }
            _ => self.regs[register],
        }
    }

    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        if offset > 0x2f || offset & 1 == 0 {
            return;
        }
        let register = (offset as usize & 0x3f) >> 1;
        match register {
            IERA | IERB => {
                self.regs[register] = value;
                self.regs[register + 2] &= value;
            }
            IPRA | IPRB | ISRA | ISRB => self.regs[register] &= value,
            TADR..=TDDR => {
                self.regs[register] = value;
                self.timer_reload[register - TADR] = value;
            }
            TSR => self.regs[TSR] = value | 0x80,
            UDR => {}
            _ => self.regs[register] = value,
        }
    }

    pub(crate) fn receive_keyboard(&mut self, value: u8) {
        self.serial.push_back(value);
        self.raise(3);
    }

    pub(crate) fn raise(&mut self, source: u8) {
        let (enable, pending, flag) = if source < 8 {
            (IERA, IPRA, 0x80 >> source)
        } else {
            (IERB, IPRB, 0x80 >> (source - 8))
        };
        if self.regs[enable] & flag != 0 {
            self.regs[pending] |= flag;
        }
    }

    pub(crate) fn tick(&mut self, cycles: u32, cpu_clock: u32) {
        const PRESCALER: [u32; 8] = [1, 10, 25, 40, 125, 160, 250, 500];
        const SOURCE: [u8; 4] = [2, 7, 10, 11];
        self.clock_fraction = self
            .clock_fraction
            .saturating_add(u64::from(cycles) * 10_000_000);
        let timer_cycles = (self.clock_fraction / u64::from(cpu_clock)) as u32;
        self.clock_fraction %= u64::from(cpu_clock);
        for channel in 0..4 {
            let mode = match channel {
                0 => self.regs[TACR],
                1 => self.regs[TBCR],
                2 => self.regs[TCDCR] >> 4,
                _ => self.regs[TCDCR],
            };
            if mode & 7 == 0 || channel == 0 && mode & 8 != 0 {
                continue;
            }
            self.timer_ticks[channel] = self.timer_ticks[channel].saturating_add(timer_cycles);
            let divisor = PRESCALER[(mode & 7) as usize];
            while self.timer_ticks[channel] >= divisor {
                self.timer_ticks[channel] -= divisor;
                let register = TADR + channel;
                self.regs[register] = self.regs[register].wrapping_sub(1);
                if self.regs[register] == 0 {
                    self.regs[register] = self.timer_reload[channel];
                    self.raise(SOURCE[channel]);
                }
            }
        }
    }

    pub(crate) fn timer_a_event(&mut self) {
        if self.regs[TACR] & 0x0f != 8 {
            return;
        }
        self.regs[TADR] = self.regs[TADR].wrapping_sub(1);
        if self.regs[TADR] == 0 {
            self.regs[TADR] = self.timer_reload[0];
            self.raise(2);
        }
    }

    pub(crate) fn has_interrupt(&self) -> bool {
        self.pending_mask(IPRA, IMRA, ISRA) != 0 || self.pending_mask(IPRB, IMRB, ISRB) != 0
    }

    /// HSync/raster/VSYNC（およびVSYNC event-count Timer A）を命令境界へ
    /// 正確に配送する必要があるかを返す。
    pub(crate) fn needs_scanline_boundaries(&self) -> bool {
        // A: source 0/1/2、B: source 9。
        self.regs[IERA] & 0xe0 != 0 || self.regs[IERB] & 0x40 != 0
    }

    pub(crate) fn acknowledge(&mut self) -> Option<u8> {
        for (pending, mask, service, base) in [(IPRA, IMRA, ISRA, 8u8), (IPRB, IMRB, ISRB, 0)] {
            let available = self.pending_mask(pending, mask, service);
            if available == 0 {
                continue;
            }
            let bit_index = available.leading_zeros() as u8;
            let flag = 0x80 >> bit_index;
            self.regs[pending] &= !flag;
            if self.regs[VR] & 8 != 0 {
                self.regs[service] |= flag;
            }
            return Some((self.regs[VR] & 0xf0) | base + (7 - bit_index));
        }
        None
    }

    fn pending_mask(&self, pending: usize, mask: usize, service: usize) -> u8 {
        self.regs[pending] & self.regs[mask] & !self.regs[service]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prioritises_and_vectors_enabled_sources() {
        let mut mfp = Mfp::default();
        mfp.write(0x17, 0x40); // VR base
        mfp.write(0x07, 0xff); // IERA
        mfp.write(0x13, 0xff); // IMRA
        mfp.raise(3);
        mfp.raise(1);
        assert!(mfp.has_interrupt());
        assert_eq!(mfp.acknowledge(), Some(0x4e));
    }

    #[test]
    fn timers_keep_same_wall_clock_rate_across_machine_profiles() {
        let mut ten_mhz = Mfp::default();
        let mut twenty_five_mhz = Mfp::default();
        for mfp in [&mut ten_mhz, &mut twenty_five_mhz] {
            mfp.write(0x07, 0x01); // IERA timer B
            mfp.write(0x13, 0x01); // IMRA timer B
            mfp.write(0x21, 1); // TBDR
            mfp.write(0x1b, 1); // TBCR /10
        }
        ten_mhz.tick(10, 10_000_000);
        twenty_five_mhz.tick(25, 25_000_000);
        assert_eq!(ten_mhz.has_interrupt(), twenty_five_mhz.has_interrupt());
        assert!(ten_mhz.has_interrupt());
    }

    #[test]
    fn queued_keyboard_bytes_each_raise_receive_interrupt() {
        let mut mfp = Mfp::default();
        mfp.write(0x07, 0x10); // IERA receive full
        mfp.write(0x13, 0x10); // IMRA receive full
        mfp.receive_keyboard(0x1e);
        mfp.receive_keyboard(0x9e);
        assert!(mfp.has_interrupt());
        assert!(mfp.acknowledge().is_some());
        assert_eq!(mfp.read(0x2f, 0xff), 0x1e);
        assert!(mfp.has_interrupt(), "second byte must remain interruptible");
        assert!(mfp.acknowledge().is_some());
        assert_eq!(mfp.read(0x2f, 0xff), 0x9e);
    }
}
