//! X68000 システムポート。

use serde::{Deserialize, Serialize};

use crate::MachineModel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SystemPort {
    contrast: u8,
    monitor: u8,
    keyboard_control: u8,
    cpu_type: u8,
    ram_wait: u8,
    rom_wait: u8,
}

impl SystemPort {
    /// 必要な初期値と依存オブジェクトを設定し、利用可能なインスタンスを構築する。
    pub(crate) fn new(model: MachineModel) -> Self {
        Self {
            contrast: 0,
            monitor: 0,
            keyboard_control: 8,
            cpu_type: match model {
                MachineModel::X68000 => 0xff,
                MachineModel::X68000Xvi => 0xfe,
                MachineModel::X68030 => 0xdc,
            },
            ram_wait: 0,
            rom_wait: 0,
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&self, offset: u32) -> u8 {
        if offset & 1 == 0 {
            return 0xff;
        }
        match (offset & 0x0f) >> 1 {
            0 => 0xf0 | self.contrast,
            1 => 0xf0 | self.monitor,
            3 => 0xf0 | self.keyboard_control,
            4 => (self.rom_wait << 4) | self.ram_wait,
            5 => self.cpu_type,
            _ => 0xff,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(&mut self, offset: u32, value: u8) -> Option<bool> {
        if offset & 1 == 0 {
            return None;
        }
        match (offset & 0x0f) >> 1 {
            0 => self.contrast = value & 0x0f,
            1 => self.monitor = value & 0x0b,
            3 => self.keyboard_control = value & 0x0e,
            4 => {
                self.ram_wait = value & 0x0f;
                self.rom_wait = value >> 4;
            }
            6 => return Some(value == 0x31),
            _ => {}
        }
        None
    }

    /// 機種・アドレス・アクセス幅に対応する追加バスクロック数を返す。
    pub(crate) fn memory_wait(&self, rom: bool) -> u32 {
        u32::from(if rom { self.rom_wait } else { self.ram_wait })
    }

    /// bit 3 はキーボードから本体へのデータ送信許可。
    pub(crate) fn keyboard_enabled(&self) -> bool {
        self.keyboard_control & 8 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `model_and_x68030_wait_port_match_machine_profile` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn model_and_x68030_wait_port_match_machine_profile() {
        let mut port = SystemPort::new(MachineModel::X68030);
        assert_eq!(port.read(0x0b), 0xdc);
        port.write(0x09, 0xa3);
        assert_eq!(port.read(0x09), 0xa3);
        assert_eq!(port.memory_wait(false), 3);
        assert_eq!(port.memory_wait(true), 10);
    }

    #[test]
    /// `keyboard_control_gates_transmission` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn keyboard_control_gates_transmission() {
        let mut port = SystemPort::new(MachineModel::X68000);
        assert!(port.keyboard_enabled());
        port.write(0x07, 0x00);
        assert!(!port.keyboard_enabled());
        port.write(0x07, 0x08);
        assert!(port.keyboard_enabled());
    }
}
