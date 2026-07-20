//! CZ-6BM1互換MIDI拡張ボード。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct MidiBoard {
    register_bank: u8,
    vector: u8,
    interrupt_enable: u8,
    interrupt_vector: u8,
    interrupt_pending: bool,
    transmit: VecDeque<u8>,
    output: VecDeque<u8>,
    transmit_cycles: u32,
    general_timer: u16,
    general_remaining: u64,
    midi_timer: u16,
    midi_remaining: u64,
    control_05: u8,
}

impl MidiBoard {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&mut self, offset: u32) -> u8 {
        match offset & 0x0f {
            0x01 => {
                self.interrupt_vector = 0x10;
                self.vector | self.interrupt_vector
            }
            0x09 if self.register_bank == 5 => {
                if self.transmit.len() >= 256 {
                    0x01
                } else {
                    0xc0
                }
            }
            _ => 0,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        match offset & 0x0f {
            0x03 => {
                self.register_bank = value & 0x0f;
                if value & 0x80 != 0 {
                    self.reset();
                }
            }
            0x09 => match self.register_bank {
                0 => self.vector = value & 0xe0,
                8 => self.general_timer = (self.general_timer & 0x3f00) | u16::from(value),
                _ => {}
            },
            0x0b => match self.register_bank {
                0 => self.control_05 = value,
                8 => {
                    self.general_timer =
                        (self.general_timer & 0xff) | (u16::from(value & 0x3f) << 8);
                    if value & 0x80 != 0 {
                        self.general_remaining = u64::from(self.general_timer) * 80;
                    }
                }
                _ => {}
            },
            0x0d => match self.register_bank {
                0 => self.interrupt_enable = value,
                5 if self.transmit.len() < 1024 => self.transmit.push_back(value),
                8 => self.midi_timer = (self.midi_timer & 0x3f00) | u16::from(value),
                _ => {}
            },
            0x0f if self.register_bank == 8 => {
                self.midi_timer = (self.midi_timer & 0xff) | (u16::from(value & 0x3f) << 8);
                if value & 0x80 != 0 {
                    self.midi_remaining = u64::from(self.midi_timer) * 80;
                }
            }
            _ => {}
        }
    }

    /// 経過CPUクロックをデバイス固有クロックへ変換し、タイマーと転送状態を進める。
    pub(crate) fn tick(&mut self, cycles: u32, clock_hz: u32) {
        self.transmit_cycles = self.transmit_cycles.saturating_add(cycles);
        let byte_cycles = (clock_hz / 3_125).max(1);
        while self.transmit_cycles >= byte_cycles {
            self.transmit_cycles -= byte_cycles;
            if let Some(byte) = self.transmit.pop_front() {
                self.output.push_back(byte);
                if self.transmit.len() < 256 && self.interrupt_enable & 0x40 != 0 {
                    self.raise(0x0c);
                }
            }
        }
        let cycles = u64::from(cycles);
        if self.general_timer != 0 {
            self.general_remaining = self.general_remaining.saturating_sub(cycles);
            if self.general_remaining == 0 {
                self.general_remaining = u64::from(self.general_timer) * 80;
                if self.interrupt_enable & 0x80 != 0 {
                    self.raise(0x0e);
                }
            }
        }
        if self.midi_timer != 0 {
            self.midi_remaining = self.midi_remaining.saturating_sub(cycles);
            if self.midi_remaining == 0 {
                self.midi_remaining = u64::from(self.midi_timer) * 80;
                if self.control_05 & 0x80 == 0 && self.interrupt_enable & 0x02 != 0 {
                    self.raise(0x02);
                }
            }
        }
    }

    /// `interrupt_pending` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn interrupt_pending(&self) -> bool {
        self.interrupt_pending
    }

    /// 割り込み状態を更新し、CPUと周辺機器のハンドシェイクを進める。
    pub(crate) fn acknowledge(&mut self) -> u8 {
        self.interrupt_pending = false;
        self.vector | self.interrupt_vector
    }

    /// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
    pub(crate) fn drain(&mut self) -> Vec<u8> {
        self.output.drain(..).collect()
    }

    /// 指定したMFP割り込み源を有効化状態に従って保留キューへ追加する。
    fn raise(&mut self, vector: u8) {
        self.interrupt_vector = vector;
        self.interrupt_pending = true;
    }

    /// 内部状態をリセットし、関連する周辺機器を起動直後の状態へ戻す。
    fn reset(&mut self) {
        let output = std::mem::take(&mut self.output);
        *self = Self::default();
        self.output = output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `bytes_leave_fifo_at_midi_baud_rate` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn bytes_leave_fifo_at_midi_baud_rate() {
        let mut midi = MidiBoard::default();
        midi.write(3, 5);
        midi.write(0x0d, 0x90);
        midi.write(0x0d, 60);
        midi.write(0x0d, 100);
        midi.tick(9_599, 10_000_000);
        assert_eq!(midi.drain(), vec![0x90, 60]);
        midi.tick(1, 10_000_000);
        assert_eq!(midi.drain(), vec![100]);
    }
}
