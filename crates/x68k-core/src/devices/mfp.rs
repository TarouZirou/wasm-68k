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
const MFP_CLOCK: u32 = 4_000_000;
const TIMER_PRESCALER: [u32; 8] = [1, 4, 10, 16, 50, 64, 100, 200];
const TIMER_SOURCE: [u8; 4] = [2, 7, 10, 11];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Mfp {
    regs: [u8; 24],
    timer_reload: [u8; 4],
    timer_ticks: [u32; 4],
    clock_fraction: u64,
    serial: VecDeque<u8>,
}

impl Default for Mfp {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
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
    /// Active Edge RegisterでGPIP入力のどちらのedgeを割り込み源にするかを返す。
    pub(crate) fn gpip_rising_edge(&self, mask: u8) -> bool {
        self.regs[1] & mask != 0
    }

    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&mut self, offset: u32, gpip: u8) -> u8 {
        if offset > 0x2f || offset & 1 == 0 {
            return 0xff;
        }
        let register = (offset as usize & 0x3f) >> 1;
        match register {
            0 => gpip,
            RSR => {
                if self.serial.is_empty() {
                    self.regs[RSR] & 0x7f
                } else {
                    self.regs[RSR] | 0x80
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

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
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

    /// MFP USARTの受信FIFOへキーボードスキャンコードを追加してIRQを立てる。
    pub(crate) fn receive_keyboard(&mut self, value: u8) {
        self.serial.push_back(value);
        self.raise(3);
    }

    /// 指定したMFP割り込み源を有効化状態に従って保留キューへ追加する。
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

    /// 経過CPUクロックをデバイス固有クロックへ変換し、タイマーと転送状態を進める。
    pub(crate) fn tick(&mut self, cycles: u32, cpu_clock: u32) {
        self.clock_fraction = self
            .clock_fraction
            .saturating_add(u64::from(cycles) * u64::from(MFP_CLOCK));
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
            let divisor = TIMER_PRESCALER[(mode & 7) as usize];
            while self.timer_ticks[channel] >= divisor {
                self.timer_ticks[channel] -= divisor;
                let register = TADR + channel;
                self.regs[register] = self.regs[register].wrapping_sub(1);
                if self.regs[register] == 0 {
                    self.regs[register] = self.timer_reload[channel];
                    self.raise(TIMER_SOURCE[channel]);
                }
            }
        }
    }

    /// 有効なdelay-mode timerが次にunderflowするまでのCPU cycle数。
    ///
    /// CPUをunderflowの先までまとめて実行すると、その区間内で本来動くISRが
    /// 遅延し、音源driver等の制御フローが変わる。現在の4MHz端数も含めた
    /// event境界を返して、CPU schedulerのslice上限に使う。
    pub(crate) fn cycles_until_next_timer_event(&self, cpu_clock: u32) -> u32 {
        let mut result = u32::MAX;
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
            let source = TIMER_SOURCE[channel];
            let enabled = if source < 8 {
                self.regs[IERA] & (0x80 >> source) != 0
            } else {
                self.regs[IERB] & (0x80 >> (source - 8)) != 0
            };
            if !enabled {
                continue;
            }
            let divisor = u64::from(TIMER_PRESCALER[(mode & 7) as usize]);
            let decrements = match self.regs[TADR + channel] {
                0 => 256,
                value => u64::from(value),
            };
            let first = divisor.saturating_sub(u64::from(self.timer_ticks[channel]));
            let mfp_clocks = first.saturating_add(divisor.saturating_mul(decrements - 1));
            let numerator = mfp_clocks
                .saturating_mul(u64::from(cpu_clock))
                .saturating_sub(self.clock_fraction);
            let cpu_cycles = numerator
                .div_ceil(u64::from(MFP_CLOCK))
                .clamp(1, u64::from(u32::MAX)) as u32;
            result = result.min(cpu_cycles);
        }
        result
    }

    /// MFPタイマーAのイベントカウント入力を1回進め、満了時は割り込みを要求する。
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

    /// `has_interrupt` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn has_interrupt(&self) -> bool {
        self.pending_mask(IPRA, IMRA, ISRA) != 0 || self.pending_mask(IPRB, IMRB, ISRB) != 0
    }

    /// HSync/raster/VSYNC（およびVSYNC event-count Timer A）を命令境界へ
    /// 正確に配送する必要があるかを返す。
    pub(crate) fn needs_scanline_boundaries(&self) -> bool {
        // A: source 0/1/2、B: source 9。
        self.regs[IERA] & 0xe0 != 0 || self.regs[IERB] & 0x40 != 0
    }

    /// 割り込み状態を更新し、CPUと周辺機器のハンドシェイクを進める。
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
            let vector = base + (7 - bit_index);
            return Some((self.regs[VR] & 0xf0) | vector);
        }
        None
    }

    /// 有効化済みで保留中の割り込み源をビット集合で返す。
    fn pending_mask(&self, pending: usize, mask: usize, service: usize) -> u8 {
        self.regs[pending] & self.regs[mask] & !self.regs[service]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `prioritises_and_vectors_enabled_sources` が想定する振る舞いを満たし、回帰がないことを検証する。
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
    /// `timers_keep_same_wall_clock_rate_across_machine_profiles` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn timers_keep_same_wall_clock_rate_across_machine_profiles() {
        let mut ten_mhz = Mfp::default();
        let mut twenty_five_mhz = Mfp::default();
        for mfp in [&mut ten_mhz, &mut twenty_five_mhz] {
            mfp.write(0x07, 0x01); // IERA timer B
            mfp.write(0x13, 0x01); // IMRA timer B
            mfp.write(0x21, 1); // TBDR
            mfp.write(0x1b, 1); // TBCR /4
        }
        // 4 MFP clocks。CPU profileが違っても同じwall-clock量を与える。
        ten_mhz.tick(10, 10_000_000);
        twenty_five_mhz.tick(25, 25_000_000);
        assert_eq!(ten_mhz.has_interrupt(), twenty_five_mhz.has_interrupt());
        assert!(ten_mhz.has_interrupt());
    }

    #[test]
    /// `scheduler_stops_at_enabled_timer_underflow` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn scheduler_stops_at_enabled_timer_underflow() {
        let mut mfp = Mfp::default();
        mfp.write(0x09, 0x10); // IERB timer D
        mfp.write(0x25, 2); // TDDR
        mfp.write(0x1d, 1); // TCDCR: timer D /4
        assert_eq!(mfp.cycles_until_next_timer_event(10_000_000), 20);
        mfp.tick(10, 10_000_000);
        assert_eq!(mfp.cycles_until_next_timer_event(10_000_000), 10);
        assert!(!mfp.has_interrupt());
        mfp.tick(10, 10_000_000);
        assert!(mfp.has_interrupt());
    }

    #[test]
    /// `queued_keyboard_bytes_each_raise_receive_interrupt` が想定する振る舞いを満たし、回帰がないことを検証する。
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

    #[test]
    /// `receive_status_reports_buffer_full_only_while_data_is_queued` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn receive_status_reports_buffer_full_only_while_data_is_queued() {
        let mut mfp = Mfp::default();
        assert_eq!(mfp.read(0x2b, 0xff) & 0x80, 0);
        mfp.receive_keyboard(0x2a);
        assert_eq!(mfp.read(0x2b, 0xff) & 0x80, 0x80);
        assert_eq!(mfp.read(0x2f, 0xff), 0x2a);
        assert_eq!(mfp.read(0x2b, 0xff) & 0x80, 0);
    }

    #[test]
    /// `aer_selects_the_active_gpip_edge` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn aer_selects_the_active_gpip_edge() {
        let mut mfp = Mfp::default();
        assert!(!mfp.gpip_rising_edge(0x10));
        mfp.write(0x03, 0x16);
        assert!(mfp.gpip_rising_edge(0x10));
    }
}
