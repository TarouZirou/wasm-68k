//! HD63450 DMAC の4チャネルレジスタと転送状態。
//!
//! レジスタ配置と制御ビットは PX68k `x68k/dmac.c` / `dmac.h` を比較資料としている。

use serde::{Deserialize, Serialize};

use crate::MachineModel;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) enum TransferWidth {
    Byte,
    Word,
    Long,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Transfer {
    pub channel: usize,
    pub source: u32,
    pub destination: u32,
    pub width: TransferWidth,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Channel {
    csr: u8,
    cer: u8,
    dcr: u8,
    ocr: u8,
    scr: u8,
    ccr: u8,
    mtc: u16,
    mar: u32,
    dar: u32,
    btc: u16,
    bar: u32,
    niv: u8,
    eiv: u8,
    mfc: u8,
    cpr: u8,
    dfc: u8,
    bfc: u8,
    gcr: u8,
    chain_started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Dma {
    channels: [Channel; 4],
    interrupt_channels: u8,
    last_interrupt: u8,
    address_mask: u32,
}

impl Default for Dma {
    fn default() -> Self {
        Self::new(MachineModel::X68000)
    }
}

impl Dma {
    pub(crate) fn new(model: MachineModel) -> Self {
        Self {
            channels: std::array::from_fn(|_| Channel::default()),
            interrupt_channels: 0,
            last_interrupt: 0,
            address_mask: if model == MachineModel::X68030 {
                u32::MAX
            } else {
                0x00ff_ffff
            },
        }
    }

    pub(crate) fn read(&self, offset: u32) -> u8 {
        if offset >= 0x100 {
            return 0;
        }
        let channel = &self.channels[((offset >> 6) & 3) as usize];
        match offset & 0x3f {
            0x00 => channel.csr,
            0x01 => channel.cer,
            0x04 => channel.dcr,
            0x05 => channel.ocr,
            0x06 => channel.scr,
            0x07 => channel.ccr,
            0x0a => (channel.mtc >> 8) as u8,
            0x0b => channel.mtc as u8,
            0x0c..=0x0f => byte_of(channel.mar, (offset & 3) as usize),
            0x14..=0x17 => byte_of(channel.dar, (offset & 3) as usize),
            0x1a => (channel.btc >> 8) as u8,
            0x1b => channel.btc as u8,
            0x1c..=0x1f => byte_of(channel.bar, (offset & 3) as usize),
            0x25 => channel.niv,
            0x27 => channel.eiv,
            0x29 => channel.mfc,
            0x2d => channel.cpr,
            0x31 => channel.dfc,
            0x39 => channel.bfc,
            0x3f => channel.gcr,
            _ => 0,
        }
    }

    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        if offset >= 0x100 {
            return;
        }
        let index = ((offset >> 6) & 3) as usize;
        let channel = &mut self.channels[index];
        match offset & 0x3f {
            0x00 => channel.csr &= !value | 0x09,
            0x01 => channel.cer &= !value,
            0x04 => channel.dcr = value,
            0x05 => channel.ocr = value,
            0x06 => channel.scr = value,
            0x07 => {
                if value & 0x10 != 0 && channel.ccr & 0x80 != 0 {
                    channel.cer = 0x11;
                    channel.csr = (channel.csr | 0x10) & !0x08;
                    channel.ccr &= !0x80;
                    self.raise(index);
                } else if value & 0x80 != 0 {
                    if channel.mtc == 0
                        && (channel.ocr & 8 == 0
                            || channel.ocr & 4 == 0 && channel.btc == 0
                            || channel.bar == 0)
                    {
                        channel.cer = 0x0d;
                        channel.csr |= 0x10;
                        self.raise(index);
                    } else {
                        channel.chain_started = false;
                        channel.ccr = (value & !0x10) | 0x80;
                        channel.csr |= 0x08;
                    }
                } else {
                    channel.ccr = (value & !0x10) | (channel.ccr & 0x80);
                }
            }
            0x0a => channel.mtc = (channel.mtc & 0x00ff) | (u16::from(value) << 8),
            0x0b => channel.mtc = (channel.mtc & 0xff00) | u16::from(value),
            0x0c..=0x0f => set_address(
                &mut channel.mar,
                (offset & 3) as usize,
                value,
                self.address_mask,
            ),
            0x14..=0x17 => set_address(
                &mut channel.dar,
                (offset & 3) as usize,
                value,
                self.address_mask,
            ),
            0x1a => channel.btc = (channel.btc & 0x00ff) | (u16::from(value) << 8),
            0x1b => channel.btc = (channel.btc & 0xff00) | u16::from(value),
            0x1c..=0x1f => set_address(
                &mut channel.bar,
                (offset & 3) as usize,
                value,
                self.address_mask,
            ),
            0x25 => channel.niv = value,
            0x27 => channel.eiv = value,
            0x29 => channel.mfc = value,
            0x2d => channel.cpr = value,
            0x31 => channel.dfc = value,
            0x39 => channel.bfc = value,
            0x3f if index == 3 => channel.gcr = value,
            _ => {}
        }
    }

    pub(crate) fn next_transfer(&self, channel: usize) -> Option<Transfer> {
        let state = self.channels.get(channel)?;
        if state.csr & 0x08 == 0 || state.ccr & 0x20 != 0 || state.mtc == 0 {
            return None;
        }
        let width = match ((state.ocr >> 4) & 3) + ((state.dcr >> 1) & 4) {
            5 => TransferWidth::Word,
            6 => TransferWidth::Long,
            _ => TransferWidth::Byte,
        };
        let (source, destination) = if state.ocr & 0x80 != 0 {
            (state.dar, state.mar)
        } else {
            (state.mar, state.dar)
        };
        Some(Transfer {
            channel,
            source,
            destination,
            width,
        })
    }

    /// 1転送を完了し、channelのTerminal Countへ到達したとき`true`を返す。
    pub(crate) fn complete(&mut self, transfer: Transfer, success: bool) -> bool {
        let channel = &mut self.channels[transfer.channel];
        if !success {
            channel.cer = 0x09;
            channel.csr = (channel.csr | 0x10) & !0x08;
            channel.ccr &= !0x80;
            self.raise(transfer.channel);
            return false;
        }
        let step = match transfer.width {
            TransferWidth::Byte => 1,
            TransferWidth::Word => 2,
            TransferWidth::Long => 4,
        };
        if channel.scr & 4 != 0 {
            channel.mar = channel.mar.wrapping_add(step);
        } else if channel.scr & 8 != 0 {
            channel.mar = channel.mar.wrapping_sub(step);
        }
        if channel.scr & 1 != 0 {
            channel.dar = channel.dar.wrapping_add(step);
        } else if channel.scr & 2 != 0 {
            channel.dar = channel.dar.wrapping_sub(step);
        }
        channel.mtc -= 1;
        if channel.mtc == 0 {
            if channel.ocr & 8 != 0 {
                if channel.ocr & 4 != 0 {
                    if channel.bar != 0 {
                        return false;
                    }
                } else {
                    channel.btc = channel.btc.saturating_sub(1);
                    if channel.btc != 0 {
                        return false;
                    }
                }
            }
            channel.csr = (channel.csr | 0x80) & !0x08;
            channel.ccr &= !0x80;
            self.raise(transfer.channel);
            return true;
        }
        false
    }

    pub(crate) fn interrupt_pending(&self) -> bool {
        self.interrupt_channels != 0
    }

    pub(crate) fn chain_descriptor_request(&self, channel: usize) -> Option<(u32, bool)> {
        let channel = self.channels.get(channel)?;
        if channel.csr & 8 == 0 || channel.ocr & 8 == 0 || channel.mtc != 0 {
            return None;
        }
        let link = channel.ocr & 4 != 0;
        if channel.bar == 0 || (!link && channel.chain_started && channel.btc == 0) {
            return None;
        }
        Some((channel.bar, link))
    }

    pub(crate) fn load_chain_descriptor(
        &mut self,
        channel: usize,
        mar: u32,
        mtc: u16,
        next_bar: Option<u32>,
    ) {
        let state = &mut self.channels[channel];
        if mtc == 0 {
            self.chain_failed(channel);
            return;
        }
        state.mar = mar & self.address_mask;
        state.mtc = mtc;
        state.chain_started = true;
        if let Some(next) = next_bar {
            state.bar = next & self.address_mask;
        } else {
            state.bar = state.bar.wrapping_add(6) & self.address_mask;
        }
    }

    pub(crate) fn chain_failed(&mut self, channel: usize) {
        let state = &mut self.channels[channel];
        state.cer = 0x0f;
        state.csr = (state.csr | 0x10) & !0x08;
        state.ccr &= !0x80;
        self.raise(channel);
    }

    pub(crate) fn acknowledge(&mut self) -> Option<u8> {
        for delta in 0..4 {
            let index = (usize::from(self.last_interrupt) + delta) & 3;
            if self.interrupt_channels & (1 << index) != 0 {
                self.interrupt_channels &= !(1 << index);
                self.last_interrupt = ((index + 1) & 3) as u8;
                let channel = &self.channels[index];
                return Some(if channel.csr & 0x10 != 0 {
                    channel.eiv
                } else {
                    channel.niv
                });
            }
        }
        None
    }

    fn raise(&mut self, channel: usize) {
        if self.channels[channel].ccr & 8 != 0 {
            self.interrupt_channels |= 1 << channel;
        }
    }
}

fn byte_of(value: u32, byte: usize) -> u8 {
    value.to_be_bytes()[byte]
}

fn set_address(target: &mut u32, byte: usize, value: u8, mask: u32) {
    let mut bytes = target.to_be_bytes();
    bytes[byte] = value;
    *target = u32::from_be_bytes(bytes) & mask;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_advances_and_completes_a_transfer() {
        let mut dma = Dma::default();
        dma.write(0x06, 4);
        dma.write(0x0b, 1);
        dma.write(0x0f, 0x20);
        dma.write(0x17, 0x40);
        dma.write(0x07, 0x88);
        let transfer = dma.next_transfer(0).unwrap();
        assert_eq!((transfer.source, transfer.destination), (0x20, 0x40));
        dma.complete(transfer, true);
        assert_eq!(dma.read(0), 0x80);
    }

    #[test]
    fn x68030_dma_preserves_full_32_bit_addresses() {
        let mut dma = Dma::new(MachineModel::X68030);
        for (offset, value) in [0x12, 0x34, 0x56, 0x78].into_iter().enumerate() {
            dma.write(0x0c + offset as u32, value);
        }
        assert_eq!(dma.channels[0].mar, 0x1234_5678);

        let mut legacy = Dma::default();
        legacy.write(0x0c, 0x12);
        legacy.write(0x0d, 0x34);
        assert_eq!(legacy.channels[0].mar, 0x0034_0000);
    }
}
