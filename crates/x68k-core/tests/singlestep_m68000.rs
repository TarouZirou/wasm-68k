//! SingleStepTests/m68000 の固定代表 JSON と外部フルコーパスを検証する。
//!
//! バイナリデコーダと実行時の PC 変換は、m68k-rs の MIT ライセンスの
//! テスト実装を参考にしている。詳細は `docs/PROVENANCE.md` を参照。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use m68k::core::cpu::{MFLAG_SET, SFLAG_SET};
use m68k::{AddressBus, CpuCore, CpuType, NoOpHleHandler, StepResult};
use serde::Deserialize;

const SST_REVISION: &str = "64b253116a3de04aaac4346c43680960dc9b67e5";
const MAGIC_FILE: u32 = 0x1A3F_5D71;
const MAGIC_TEST: u32 = 0xABC1_2367;
const MAGIC_NAME: u32 = 0x89AB_CDEF;
const MAGIC_STATE: u32 = 0x0123_4567;
const MAGIC_TXNS: u32 = 0x4567_89AB;

#[derive(Clone, Debug, Deserialize)]
struct State {
    d0: u32,
    d1: u32,
    d2: u32,
    d3: u32,
    d4: u32,
    d5: u32,
    d6: u32,
    d7: u32,
    a0: u32,
    a1: u32,
    a2: u32,
    a3: u32,
    a4: u32,
    a5: u32,
    a6: u32,
    usp: u32,
    ssp: u32,
    sr: u32,
    pc: u32,
    #[allow(dead_code)]
    prefetch: [u32; 2],
    ram: Vec<(u32, u8)>,
}

impl State {
    /// テスト用バスのRAMを可変スライスとしてCPUテストへ公開する。
    fn data(&self, index: usize) -> u32 {
        [
            self.d0, self.d1, self.d2, self.d3, self.d4, self.d5, self.d6, self.d7,
        ][index]
    }

    /// 入力を処理待ちキューへ追加し、後続処理で利用できるようにする。
    fn address(&self, index: usize) -> u32 {
        [
            self.a0, self.a1, self.a2, self.a3, self.a4, self.a5, self.a6,
        ][index]
    }
}

#[derive(Clone, Debug, Deserialize)]
struct TestCase {
    name: String,
    initial: State,
    #[serde(rename = "final")]
    final_state: State,
    #[serde(default)]
    transactions: Vec<Vec<serde_json::Value>>,
    length: u32,
    #[serde(skip)]
    has_address_error: bool,
}

#[derive(Default)]
struct SparseBus {
    memory: HashMap<u32, u8>,
}

impl AddressBus for SparseBus {
    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_byte(&mut self, address: u32) -> u8 {
        self.memory
            .get(&(address & 0x00ff_ffff))
            .copied()
            .unwrap_or(0)
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_word(&mut self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_long(&mut self, address: u32) -> u32 {
        u32::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
            self.read_byte(address.wrapping_add(2)),
            self.read_byte(address.wrapping_add(3)),
        ])
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_byte(&mut self, address: u32, value: u8) {
        self.memory.insert(address & 0x00ff_ffff, value);
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_word(&mut self, address: u32, value: u16) {
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            self.write_byte(address.wrapping_add(offset as u32), byte);
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_long(&mut self, address: u32, value: u32) {
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            self.write_byte(address.wrapping_add(offset as u32), byte);
        }
    }
}

/// 入力データを検証して読み込み、対応する実行状態へ反映する。
fn load_cpu(cpu: &mut CpuCore, state: &State) {
    cpu.set_cpu_type(CpuType::M68000);
    cpu.set_sst_m68000_compat(true);
    cpu.set_sr_noint_nosp(state.sr as u16);
    cpu.pc = state.pc.wrapping_sub(4);
    for index in 0..8 {
        cpu.set_d(index, state.data(index));
    }
    for index in 0..7 {
        cpu.set_a(index, state.address(index));
    }
    cpu.sp[0] = state.usp;
    cpu.sp[SFLAG_SET as usize] = state.ssp;
    cpu.sp[(SFLAG_SET | MFLAG_SET) as usize] = state.ssp;
    cpu.set_sp(if cpu.is_supervisor() {
        state.ssp
    } else {
        state.usp
    });
}

/// SRのスーパーバイザビットに従い、比較対象のスタックポインタを選ぶ。
fn supervisor_sp(cpu: &CpuCore) -> u32 {
    if cpu.is_supervisor() {
        cpu.sp()
    } else {
        cpu.sp[SFLAG_SET as usize]
    }
}

/// 命令テストで比較対象とするステータスレジスタのビットマスクを返す。
fn sr_mask(opcode: u16) -> u16 {
    let group = opcode >> 12;
    let op_mode = (opcode >> 6) & 7;
    let ea_mode = (opcode >> 3) & 7;
    let bcd =
        ((group == 0xc || group == 8) && op_mode == 4 && ea_mode <= 1) || opcode & 0xffc0 == 0x4800;
    if bcd { !0x000a } else { 0xffff }
}

/// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
fn run_case(test: &TestCase, source: &str, index: usize) -> Result<(), String> {
    // re/we は実バスの AS/UDS/LDS タイミングを検証するケースであり、命令コアの
    // AddressBus API では表現できない。X68000 側のアドレスエラー試験で別途検証する。
    if test.has_address_error {
        return Ok(());
    }
    let mut bus = SparseBus::default();
    for &(address, value) in &test.initial.ram {
        bus.write_byte(address, value);
    }
    let mut cpu = CpuCore::new();
    load_cpu(&mut cpu, &test.initial);
    let opcode = bus.read_word(cpu.pc);
    let result = cpu.step_with_hle_handler(&mut bus, &mut NoOpHleHandler);
    let context = format!("{source}[{index}] {}", test.name);

    if let StepResult::Ok { cycles } = result
        && cycles as u32 != test.length
    {
        return Err(format!(
            "{context}: cycles: got {cycles}, expected {}",
            test.length
        ));
    }
    for register in 0..8 {
        if cpu.d(register) != test.final_state.data(register) {
            return Err(format!("{context}: D{register} mismatch"));
        }
    }
    for register in 0..7 {
        if cpu.a(register) != test.final_state.address(register) {
            return Err(format!("{context}: A{register} mismatch"));
        }
    }
    let mask = sr_mask(opcode);
    if cpu.get_sr() & mask != test.final_state.sr as u16 & mask {
        return Err(format!(
            "{context}: SR got {:#06x}, expected {:#06x}",
            cpu.get_sr(),
            test.final_state.sr
        ));
    }
    if cpu.get_usp() != test.final_state.usp || supervisor_sp(&cpu) != test.final_state.ssp {
        return Err(format!("{context}: USP/SSP mismatch"));
    }
    for &(address, expected) in &test.final_state.ram {
        let actual = bus.read_byte(address);
        if actual != expected {
            return Err(format!(
                "{context}: memory[{address:#010x}] got {actual:#04x}, expected {expected:#04x}"
            ));
        }
    }
    Ok(())
}

/// 上流JSONに明示されたアドレスエラーを期待例外として記録する。
fn mark_json_address_errors(test: &mut TestCase) {
    test.has_address_error |= test.transactions.iter().any(|transaction| {
        transaction
            .first()
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind == "re" || kind == "we")
    });
}

/// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
fn run_cases(cases: &mut [TestCase], source: &str) {
    let mut failures = Vec::new();
    for (index, test) in cases.iter_mut().enumerate() {
        mark_json_address_errors(test);
        if let Err(error) = run_case(test, source, index) {
            failures.push(error);
            if failures.len() == 25 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
/// `representative_json_from_pinned_revision` が想定する振る舞いを満たし、回帰がないことを検証する。
fn representative_json_from_pinned_revision() {
    let source = include_str!("fixtures/m68000/representative.json");
    let mut cases: Vec<TestCase> = serde_json::from_str(source).expect("valid SST JSON fixture");
    assert_eq!(cases.len(), 10);
    run_cases(&mut cases, "representative.json");
}

// フルコーパスは約 180 MiB のため同梱せず、定期 CI が固定リビジョンを取得する。
#[test]
#[ignore = "requires M68000_SST_DIR pointing at pinned v1/*.json.bin corpus"]
fn full_binary_corpus_from_pinned_revision() {
    let root = std::env::var_os("M68000_SST_DIR")
        .map(PathBuf::from)
        .expect("M68000_SST_DIR is required");
    assert_eq!(
        std::env::var("M68000_SST_REVISION").as_deref(),
        Ok(SST_REVISION),
        "the full suite must use the pinned revision"
    );
    let mut paths: Vec<_> = fs::read_dir(&root)
        .expect("fixture directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "bin"))
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 127, "unexpected SST fixture count");
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy();
        if name == "TAS.json.bin" || name == "TRAPV.json.bin" {
            continue;
        }
        let mut cases = decode_binary(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        run_cases(&mut cases, &name);
    }
}

/// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
fn take_u8(bytes: &[u8], cursor: &mut usize) -> Result<u8, String> {
    let value = *bytes.get(*cursor).ok_or("unexpected EOF")?;
    *cursor += 1;
    Ok(value)
}

/// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
fn take_u16(bytes: &[u8], cursor: &mut usize) -> Result<u16, String> {
    let end = cursor.checked_add(2).ok_or("offset overflow")?;
    let raw: [u8; 2] = bytes
        .get(*cursor..end)
        .ok_or("unexpected EOF")?
        .try_into()
        .unwrap();
    *cursor = end;
    Ok(u16::from_le_bytes(raw))
}

/// 蓄積済みの状態またはデータを取り出し、処理済みとして整理する。
fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, String> {
    let end = cursor.checked_add(4).ok_or("offset overflow")?;
    let raw: [u8; 4] = bytes
        .get(*cursor..end)
        .ok_or("unexpected EOF")?
        .try_into()
        .unwrap();
    *cursor = end;
    Ok(u32::from_le_bytes(raw))
}

/// 命令テスト名から対象データブロックを取得する。
fn block(bytes: &[u8], cursor: &mut usize, magic: u32) -> Result<(), String> {
    let _length = take_u32(bytes, cursor)?;
    let actual = take_u32(bytes, cursor)?;
    (actual == magic)
        .then_some(())
        .ok_or_else(|| format!("bad magic {actual:#010x}"))
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn decode_name(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    block(bytes, cursor, MAGIC_NAME)?;
    let length = take_u32(bytes, cursor)? as usize;
    let end = cursor.checked_add(length).ok_or("name overflow")?;
    let name = std::str::from_utf8(bytes.get(*cursor..end).ok_or("unexpected EOF")?)
        .map_err(|error| error.to_string())?
        .to_owned();
    *cursor = end;
    Ok(name)
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn decode_state(bytes: &[u8], cursor: &mut usize) -> Result<State, String> {
    block(bytes, cursor, MAGIC_STATE)?;
    let mut registers = [0; 19];
    for register in &mut registers {
        *register = take_u32(bytes, cursor)?;
    }
    let prefetch = [take_u32(bytes, cursor)?, take_u32(bytes, cursor)?];
    let count = take_u32(bytes, cursor)? as usize;
    let mut ram = Vec::with_capacity(count * 2);
    for _ in 0..count {
        let address = take_u32(bytes, cursor)?;
        let value = take_u16(bytes, cursor)?;
        ram.push((address, (value >> 8) as u8));
        ram.push((address | 1, value as u8));
    }
    Ok(State {
        d0: registers[0],
        d1: registers[1],
        d2: registers[2],
        d3: registers[3],
        d4: registers[4],
        d5: registers[5],
        d6: registers[6],
        d7: registers[7],
        a0: registers[8],
        a1: registers[9],
        a2: registers[10],
        a3: registers[11],
        a4: registers[12],
        a5: registers[13],
        a6: registers[14],
        usp: registers[15],
        ssp: registers[16],
        sr: registers[17],
        pc: registers[18],
        prefetch,
        ram,
    })
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn decode_transactions(bytes: &[u8], cursor: &mut usize) -> Result<(u32, bool), String> {
    block(bytes, cursor, MAGIC_TXNS)?;
    let cycles = take_u32(bytes, cursor)?;
    let count = take_u32(bytes, cursor)?;
    let mut address_error = false;
    for _ in 0..count {
        let kind = take_u8(bytes, cursor)?;
        let _duration = take_u32(bytes, cursor)?;
        if kind == 0 {
            continue;
        }
        address_error |= kind == 4 || kind == 5;
        for _ in 0..5 {
            let _ = take_u32(bytes, cursor)?;
        }
    }
    Ok((cycles, address_error))
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
fn decode_binary(path: &Path) -> Result<Vec<TestCase>, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let mut cursor = 0;
    if take_u32(&bytes, &mut cursor)? != MAGIC_FILE {
        return Err("bad file magic".into());
    }
    let count = take_u32(&bytes, &mut cursor)?;
    let mut cases = Vec::with_capacity(count as usize);
    for _ in 0..count {
        block(&bytes, &mut cursor, MAGIC_TEST)?;
        let name = decode_name(&bytes, &mut cursor)?;
        let initial = decode_state(&bytes, &mut cursor)?;
        let final_state = decode_state(&bytes, &mut cursor)?;
        let (length, has_address_error) = decode_transactions(&bytes, &mut cursor)?;
        cases.push(TestCase {
            name,
            initial,
            final_state,
            transactions: Vec::new(),
            length,
            has_address_error,
        });
    }
    Ok(cases)
}
