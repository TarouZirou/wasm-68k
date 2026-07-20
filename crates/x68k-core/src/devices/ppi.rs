//! uPD8255 PPI（ジョイスティック2ポートとADPCM制御）。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Ppi {
    joystick: [u8; 2],
    port_c: u8,
    control: u8,
}

impl Default for Ppi {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            joystick: [0xff; 2],
            port_c: 0x0b,
            control: 0,
        }
    }
}

impl Ppi {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&self, offset: u32) -> u8 {
        match offset & 7 {
            1 => self.joystick[0],
            3 => self.joystick[1],
            5 => self.port_c,
            7 => self.control,
            _ => 0xff,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        match offset & 7 {
            1 => self.joystick[0] = value,
            3 => self.joystick[1] = value,
            5 => self.port_c = value,
            7 if value & 0x80 == 0 => {
                let mask = 1 << ((value >> 1) & 7);
                if value & 1 != 0 {
                    self.port_c |= mask;
                } else {
                    self.port_c &= !mask;
                }
            }
            7 => self.control = value,
            _ => {}
        }
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_button(&mut self, port: u8, button: u8, pressed: bool) {
        let port = usize::from(port.min(1));
        let bit = 4 + button.min(3);
        self.set_active_low(port, bit, pressed);
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_axis(&mut self, port: u8, axis: u8, value: i16) {
        let port = usize::from(port.min(1));
        let (negative, positive) = if axis & 1 == 0 { (2, 3) } else { (0, 1) };
        self.set_active_low(port, negative, value < -8_192);
        self.set_active_low(port, positive, value > 8_192);
    }

    /// 現在のレジスタ値または入力から `port_c` に対応する描画・転送情報を算出して返す。
    pub(crate) fn port_c(&self) -> u8 {
        self.port_c
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    fn set_active_low(&mut self, port: usize, bit: u8, active: bool) {
        let mask = 1 << bit;
        if active {
            self.joystick[port] &= !mask;
        } else {
            self.joystick[port] |= mask;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `joystick_is_active_low_and_axis_has_dead_zone` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn joystick_is_active_low_and_axis_has_dead_zone() {
        let mut ppi = Ppi::default();
        ppi.set_axis(0, 0, -20_000);
        ppi.set_button(0, 0, true);
        assert_eq!(ppi.read(1) & 0x14, 0);
        ppi.set_axis(0, 0, 0);
        ppi.set_button(0, 0, false);
        assert_eq!(ppi.read(1) & 0x1c, 0x1c);
    }
}
