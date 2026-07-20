//! m68k クレート統合のスモークテスト。
//!
//! フラットメモリ上で小さな 68000 プログラムを実行し、
//! CPU コアが期待通り動作することを確認する (Phase 1 の本格検証の前段)。

use m68k::{AddressBus, CpuCore};

/// 単純なフラットメモリバス (アドレスはメモリサイズで折り返す)。
struct FlatMem {
    mem: Vec<u8>,
}

impl FlatMem {
    /// 必要な初期値と依存オブジェクトを設定し、利用可能なインスタンスを構築する。
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_word_at(&mut self, addr: u32, value: u16) {
        self.write_byte(addr, (value >> 8) as u8);
        self.write_byte(addr + 1, value as u8);
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_long_at(&mut self, addr: u32, value: u32) {
        self.write_word_at(addr, (value >> 16) as u16);
        self.write_word_at(addr + 2, value as u16);
    }
}

impl AddressBus for FlatMem {
    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_byte(&mut self, address: u32) -> u8 {
        self.mem[(address as usize) % self.mem.len()]
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_word(&mut self, address: u32) -> u16 {
        let hi = self.read_byte(address);
        let lo = self.read_byte(address.wrapping_add(1));
        u16::from_be_bytes([hi, lo])
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_long(&mut self, address: u32) -> u32 {
        let hi = u32::from(self.read_word(address));
        let lo = u32::from(self.read_word(address.wrapping_add(2)));
        (hi << 16) | lo
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_byte(&mut self, address: u32, value: u8) {
        let len = self.mem.len();
        self.mem[(address as usize) % len] = value;
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_word(&mut self, address: u32, value: u16) {
        self.write_byte(address, (value >> 8) as u8);
        self.write_byte(address.wrapping_add(1), value as u8);
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_long(&mut self, address: u32, value: u32) {
        self.write_word(address, (value >> 16) as u16);
        self.write_word(address.wrapping_add(2), value as u16);
    }
}

#[test]
/// `reset_loads_vectors` が想定する振る舞いを満たし、回帰がないことを検証する。
fn reset_loads_vectors() {
    let mut mem = FlatMem::new(0x2000);
    mem.write_long_at(0x0000, 0x0000_1800); // 初期 SSP
    mem.write_long_at(0x0004, 0x0000_0010); // 初期 PC

    let mut cpu = CpuCore::new();
    cpu.reset(&mut mem);

    assert_eq!(cpu.pc, 0x10);
    assert_eq!(cpu.sp(), 0x1800);
}

#[test]
/// `executes_arithmetic_program` が想定する振る舞いを満たし、回帰がないことを検証する。
fn executes_arithmetic_program() {
    let mut mem = FlatMem::new(0x2000);
    mem.write_long_at(0x0000, 0x0000_1800);
    mem.write_long_at(0x0004, 0x0000_0010);

    // 0x10: move.l #0x12345678, d0  (203C 1234 5678)
    // 0x16: moveq #42, d1           (722A)
    // 0x18: add.l d1, d0            (D081)
    // 0x1A: bra.s *                 (60FE) 無限ループ
    mem.write_word_at(0x10, 0x203C);
    mem.write_long_at(0x12, 0x1234_5678);
    mem.write_word_at(0x16, 0x722A);
    mem.write_word_at(0x18, 0xD081);
    mem.write_word_at(0x1A, 0x60FE);

    let mut cpu = CpuCore::new();
    cpu.reset(&mut mem);
    let consumed = cpu.execute(&mut mem, 200);

    assert!(consumed > 0);
    assert_eq!(cpu.d(0), 0x1234_5678 + 42);
    assert_eq!(cpu.d(1), 42);
}

#[test]
/// `executes_human68k_style_bsr_word` が想定する振る舞いを満たし、回帰がないことを検証する。
fn executes_human68k_style_bsr_word() {
    let mut mem = FlatMem::new(0x1_0000);
    mem.write_long_at(0x0000, 0x0000_f000);
    mem.write_long_at(0x0004, 0x0000_769c);
    // Human68k起動コードと同じBSR.W $6c7a。復帰先は$76a0。
    mem.write_word_at(0x769c, 0x6100);
    mem.write_word_at(0x769e, 0xf5dc);
    mem.write_word_at(0x6c7a, 0x60fe);

    let mut cpu = CpuCore::new();
    cpu.reset(&mut mem);
    cpu.execute(&mut mem, 100);

    assert_eq!(cpu.pc, 0x6c7a);
    assert_eq!(cpu.sp(), 0xeffc);
    assert_eq!(mem.read_long(0xeffc), 0x76a0);
}
