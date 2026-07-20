//! Z8530 SCCのマウス用channel B。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Scc {
    registers_b: [u8; 16],
    selected_a: Option<u8>,
    selected_b: Option<u8>,
    vector: u8,
    receive: VecDeque<u8>,
    mouse_dx: i16,
    mouse_dy: i16,
    mouse_buttons: u8,
}

impl Scc {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&mut self, offset: u32) -> u8 {
        match offset & 7 {
            1 => {
                self.selected_b = None;
                u8::from(!self.receive.is_empty())
            }
            3 => self.receive.pop_front().unwrap_or(0),
            5 => {
                let result = match self.selected_a.unwrap_or(0) {
                    0 => 4,
                    3 => u8::from(!self.receive.is_empty()) * 4,
                    _ => 0,
                };
                self.selected_a = None;
                result
            }
            _ => 0,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        match offset & 7 {
            1 => {
                if let Some(register) = self.selected_b.take() {
                    if register == 5
                        && self.registers_b[5] & 2 == 0
                        && value & 2 != 0
                        && self.registers_b[3] & 1 != 0
                        && self.receive.is_empty()
                    {
                        self.latch_mouse_packet();
                    }
                    if register == 2 {
                        self.vector = value;
                    }
                    self.registers_b[usize::from(register)] = value;
                } else if value & 0xf0 == 0 {
                    self.selected_b = Some(value & 0x0f);
                }
            }
            5 => {
                if let Some(register) = self.selected_a.take() {
                    match register {
                        2 => {
                            self.registers_b[2] = value;
                            self.vector = value;
                        }
                        9 => self.registers_b[9] = value,
                        _ => {}
                    }
                } else if value & 0x0f != 0 {
                    self.selected_a = Some(value & 0x0f);
                }
            }
            _ => {}
        }
    }

    /// ホストの相対移動量をX68000マウスカウンタへ加算する。
    pub(crate) fn move_mouse(&mut self, dx: i16, dy: i16) {
        self.mouse_dx = self.mouse_dx.saturating_add(dx);
        self.mouse_dy = self.mouse_dy.saturating_add(dy);
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_button(&mut self, button: u8, pressed: bool) {
        let mask = 1u8.checked_shl(u32::from(button.min(7))).unwrap_or(0);
        if pressed {
            self.mouse_buttons |= mask;
        } else {
            self.mouse_buttons &= !mask;
        }
    }

    /// SCCが保持する現在のX68000マウスボタンビットを返す。
    pub(crate) fn mouse_buttons(&self) -> u8 {
        // 診断JSON用。packetをlatchしなくてもfrontendの変換結果を検査できる。
        self.mouse_buttons
    }

    /// `interrupt_pending` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn interrupt_pending(&self) -> bool {
        let receive_mode = self.registers_b[1] & 0x18;
        self.registers_b[9] & 0x08 != 0
            && ((!self.receive.is_empty() && receive_mode == 0x10)
                || (self.receive.len() == 3 && receive_mode == 0x08))
    }

    /// 割り込み状態を更新し、CPUと周辺機器のハンドシェイクを進める。
    pub(crate) fn acknowledge(&self) -> u8 {
        if self.registers_b[9] & 2 != 0 {
            return 0xff;
        }
        if self.registers_b[9] & 1 != 0 {
            if self.registers_b[9] & 0x10 != 0 {
                return (self.vector & 0x8f).wrapping_add(0x20);
            }
            return (self.vector & 0xf1).wrapping_add(4);
        }
        self.vector
    }

    /// 現在の移動量とボタン状態をSCCマウスパケットとしてラッチする。
    fn latch_mouse_packet(&mut self) {
        let dx = self.mouse_dx.clamp(-128, 127) as i8;
        let dy = self.mouse_dy.clamp(-128, 127) as i8;
        self.mouse_dx -= i16::from(dx);
        self.mouse_dy -= i16::from(dy);
        self.receive
            .extend([self.mouse_buttons, dx as u8, dy as u8]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `rts_edge_latches_three_byte_mouse_packet` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn rts_edge_latches_three_byte_mouse_packet() {
        let mut scc = Scc::default();
        scc.write(1, 3);
        scc.write(1, 1);
        scc.move_mouse(12, -7);
        scc.set_button(0, true);
        scc.write(1, 5);
        scc.write(1, 2);
        assert_eq!(scc.read(1), 1);
        assert_eq!([scc.read(3), scc.read(3), scc.read(3)], [1, 12, 249]);
    }
}
