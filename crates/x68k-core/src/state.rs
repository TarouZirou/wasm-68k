//! バージョン付き保存状態。

use m68k::{CpuCore, CpuType};
use serde::{Deserialize, Serialize};

use crate::bus::Bus;
use crate::{MachineError, MachineModel};

pub const MAGIC: &[u8; 4] = b"X68S";
pub const VERSION: u16 = 10;
const FIXED_HEADER_SIZE: usize = 4 + 2 + 1 + 1;
const HASH_SIZE: usize = 32;
// A 12 MiB machine plus VRAM/devices currently serializes well below this
// bound.  Reject an attacker-controlled LZ4 size prefix before the decoder
// allocates it (save states can be imported from an untrusted browser file).
const MAX_DECODED_STATE_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatePayload {
    pub cpu: CpuSnapshot,
    pub bus: Bus,
    pub frame_count: u64,
    pub audio_remainder: u32,
    pub cycle_remainder: u32,
    pub cpu_cycle_debt: u32,
    pub paused: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CpuSnapshot {
    dar: [u32; 16],
    dar_save: [u32; 16],
    sr_save: u16,
    ppc: u32,
    pc: u32,
    sp: [u32; 8],
    vbr: u32,
    sfc: u32,
    dfc: u32,
    cacr: u32,
    caar: u32,
    fpr: [f64; 8],
    fpiar: u32,
    fpsr: u32,
    fpcr: u32,
    flags: [u32; 11],
    stopped: u32,
    change_of_flow: bool,
    pref_addr: u32,
    pref_data: u32,
    instr_mode: u32,
    run_mode: u32,
    exception_processing: bool,
    pmmu_enabled: bool,
    fpu_just_reset: bool,
    reset_cycles: u32,
    virq_state: u32,
    nmi_pending: u32,
    mmu: [u32; 20],
    mmu_sr: u16,
    cycles_remaining: i32,
    initial_cycles: i32,
    sst_m68000_compat: bool,
}

impl CpuSnapshot {
    /// CPU・メモリ・周辺機器・媒体差分を保存状態スナップショットへ収集する。
    pub fn capture(cpu: &CpuCore) -> Self {
        Self {
            dar: cpu.dar,
            dar_save: cpu.dar_save,
            sr_save: cpu.sr_save,
            ppc: cpu.ppc,
            pc: cpu.pc,
            sp: cpu.sp,
            vbr: cpu.vbr,
            sfc: cpu.sfc,
            dfc: cpu.dfc,
            cacr: cpu.cacr,
            caar: cpu.caar,
            fpr: cpu.fpr,
            fpiar: cpu.fpiar,
            fpsr: cpu.fpsr,
            fpcr: cpu.fpcr,
            flags: [
                cpu.t1_flag,
                cpu.t0_flag,
                cpu.s_flag,
                cpu.m_flag,
                cpu.x_flag,
                cpu.n_flag,
                cpu.not_z_flag,
                cpu.v_flag,
                cpu.c_flag,
                cpu.int_mask,
                cpu.int_level,
            ],
            stopped: cpu.stopped,
            change_of_flow: cpu.change_of_flow,
            pref_addr: cpu.pref_addr,
            pref_data: cpu.pref_data,
            instr_mode: cpu.instr_mode,
            run_mode: cpu.run_mode,
            exception_processing: cpu.exception_processing,
            pmmu_enabled: cpu.pmmu_enabled,
            fpu_just_reset: cpu.fpu_just_reset,
            reset_cycles: cpu.reset_cycles,
            virq_state: cpu.virq_state,
            nmi_pending: cpu.nmi_pending,
            mmu: [
                cpu.mmu_crp_aptr,
                cpu.mmu_crp_limit,
                cpu.mmu_srp_aptr,
                cpu.mmu_srp_limit,
                cpu.mmu_tc,
                cpu.mmu_tt0,
                cpu.mmu_tt1,
                cpu.urp,
                cpu.srp,
                cpu.tc,
                cpu.mmusr,
                cpu.dacr0,
                cpu.dacr1,
                cpu.iacr0,
                cpu.iacr1,
                cpu.itt0,
                cpu.itt1,
                cpu.dtt0,
                cpu.dtt1,
                0,
            ],
            mmu_sr: cpu.mmu_sr,
            cycles_remaining: cpu.cycles_remaining,
            initial_cycles: cpu.initial_cycles,
            sst_m68000_compat: cpu.sst_m68000_compat,
        }
    }

    /// 入力データを検証して読み込み、対応する実行状態へ反映する。
    pub fn restore(self, model: MachineModel) -> CpuCore {
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(match model {
            MachineModel::X68000 | MachineModel::X68000Xvi => CpuType::M68000,
            MachineModel::X68030 => CpuType::M68EC030,
        });
        cpu.dar = self.dar;
        cpu.dar_save = self.dar_save;
        cpu.sr_save = self.sr_save;
        cpu.ppc = self.ppc;
        cpu.pc = self.pc;
        cpu.sp = self.sp;
        cpu.vbr = self.vbr;
        cpu.sfc = self.sfc;
        cpu.dfc = self.dfc;
        cpu.cacr = self.cacr;
        cpu.caar = self.caar;
        cpu.fpr = self.fpr;
        cpu.fpiar = self.fpiar;
        cpu.fpsr = self.fpsr;
        cpu.fpcr = self.fpcr;
        cpu.t1_flag = self.flags[0];
        cpu.t0_flag = self.flags[1];
        cpu.s_flag = self.flags[2];
        cpu.m_flag = self.flags[3];
        cpu.x_flag = self.flags[4];
        cpu.n_flag = self.flags[5];
        cpu.not_z_flag = self.flags[6];
        cpu.v_flag = self.flags[7];
        cpu.c_flag = self.flags[8];
        cpu.int_mask = self.flags[9];
        cpu.int_level = self.flags[10];
        cpu.stopped = self.stopped;
        cpu.change_of_flow = self.change_of_flow;
        cpu.pref_addr = self.pref_addr;
        cpu.pref_data = self.pref_data;
        cpu.instr_mode = self.instr_mode;
        cpu.run_mode = self.run_mode;
        cpu.exception_processing = self.exception_processing;
        cpu.pmmu_enabled = self.pmmu_enabled;
        cpu.fpu_just_reset = self.fpu_just_reset;
        cpu.reset_cycles = self.reset_cycles;
        cpu.virq_state = self.virq_state;
        cpu.nmi_pending = self.nmi_pending;
        cpu.mmu_crp_aptr = self.mmu[0];
        cpu.mmu_crp_limit = self.mmu[1];
        cpu.mmu_srp_aptr = self.mmu[2];
        cpu.mmu_srp_limit = self.mmu[3];
        cpu.mmu_tc = self.mmu[4];
        cpu.mmu_tt0 = self.mmu[5];
        cpu.mmu_tt1 = self.mmu[6];
        cpu.urp = self.mmu[7];
        cpu.srp = self.mmu[8];
        cpu.tc = self.mmu[9];
        cpu.mmusr = self.mmu[10];
        cpu.dacr0 = self.mmu[11];
        cpu.dacr1 = self.mmu[12];
        cpu.iacr0 = self.mmu[13];
        cpu.iacr1 = self.mmu[14];
        cpu.itt0 = self.mmu[15];
        cpu.itt1 = self.mmu[16];
        cpu.dtt0 = self.mmu[17];
        cpu.dtt1 = self.mmu[18];
        cpu.mmu_sr = self.mmu_sr;
        cpu.cycles_remaining = self.cycles_remaining;
        cpu.initial_cycles = self.initial_cycles;
        cpu.set_sst_m68000_compat(self.sst_m68000_compat);
        cpu
    }
}

/// 現在の状態を外部で扱える形式へ変換して出力する。
pub(crate) fn encode(
    payload: &StatePayload,
    model: MachineModel,
    content: &[(String, [u8; 32])],
) -> Result<Vec<u8>, MachineError> {
    if content.len() > u8::MAX as usize
        || content
            .iter()
            .any(|(slot, _)| slot.len() > u8::MAX as usize)
    {
        return Err(MachineError::InvalidState(
            "too many or overlong ROM/media identity entries".into(),
        ));
    }
    let serialized = postcard::to_allocvec(payload)
        .map_err(|error| MachineError::InvalidState(error.to_string()))?;
    let compressed = lz4_flex::compress_prepend_size(&serialized);
    let checksum = crc32fast::hash(&compressed);
    let manifest_size = content
        .iter()
        .map(|(slot, _)| 1 + slot.len() + HASH_SIZE)
        .sum::<usize>();
    let mut state = Vec::with_capacity(FIXED_HEADER_SIZE + manifest_size + 4 + compressed.len());
    state.extend_from_slice(MAGIC);
    state.extend_from_slice(&VERSION.to_le_bytes());
    state.push(model as u8);
    state.push(content.len() as u8);
    for (slot, digest) in content {
        state.push(slot.len() as u8);
        state.extend_from_slice(slot.as_bytes());
        state.extend_from_slice(digest);
    }
    state.extend_from_slice(&checksum.to_le_bytes());
    state.extend_from_slice(&compressed);
    Ok(state)
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
pub(crate) fn decode(
    bytes: &[u8],
    current_model: MachineModel,
    expected_content: &[(String, [u8; 32])],
) -> Result<StatePayload, MachineError> {
    if bytes.len() < FIXED_HEADER_SIZE + 4 || &bytes[..4] != MAGIC {
        return Err(MachineError::InvalidState("missing X68S header".into()));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != VERSION {
        return Err(MachineError::InvalidState(format!(
            "unsupported version {version}"
        )));
    }
    let state_model = match bytes[6] {
        0 => MachineModel::X68000,
        1 => MachineModel::X68000Xvi,
        2 => MachineModel::X68030,
        value => return Err(MachineError::InvalidState(format!("unknown model {value}"))),
    };
    if state_model != current_model {
        return Err(MachineError::StateModelMismatch {
            state_model,
            current_model,
        });
    }
    let (content, payload_offset) = decode_manifest(bytes)?;
    if content != expected_content {
        return Err(MachineError::StateMediaMismatch);
    }
    let crc_end = payload_offset
        .checked_add(4)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| MachineError::InvalidState("truncated save-state checksum".into()))?;
    let expected_crc = u32::from_le_bytes(
        bytes[payload_offset..crc_end]
            .try_into()
            .expect("four-byte checksum"),
    );
    let compressed = &bytes[crc_end..];
    if crc32fast::hash(compressed) != expected_crc {
        return Err(MachineError::InvalidState("CRC mismatch".into()));
    }
    let advertised_size = compressed
        .get(..4)
        .map(|prefix| {
            u32::from_le_bytes(prefix.try_into().expect("four-byte size prefix")) as usize
        })
        .ok_or_else(|| MachineError::InvalidState("truncated LZ4 payload".into()))?;
    if advertised_size > MAX_DECODED_STATE_SIZE {
        return Err(MachineError::InvalidState(format!(
            "decompressed state is too large ({advertised_size} bytes)"
        )));
    }
    let decoded = lz4_flex::decompress_size_prepended(compressed)
        .map_err(|error| MachineError::InvalidState(error.to_string()))?;
    postcard::from_bytes(&decoded).map_err(|error| MachineError::InvalidState(error.to_string()))
}

/// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
pub(crate) fn decode_manifest(
    bytes: &[u8],
) -> Result<(Vec<(String, [u8; 32])>, usize), MachineError> {
    let count = usize::from(bytes[7]);
    let mut cursor = FIXED_HEADER_SIZE;
    let mut content = Vec::with_capacity(count);
    for _ in 0..count {
        let slot_len =
            usize::from(*bytes.get(cursor).ok_or_else(|| {
                MachineError::InvalidState("truncated ROM/media manifest".into())
            })?);
        cursor += 1;
        let slot_end = cursor
            .checked_add(slot_len)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| MachineError::InvalidState("truncated ROM/media slot".into()))?;
        let slot = std::str::from_utf8(&bytes[cursor..slot_end])
            .map_err(|_| MachineError::InvalidState("invalid ROM/media slot name".into()))?
            .to_owned();
        cursor = slot_end;
        let hash_end = cursor
            .checked_add(HASH_SIZE)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| MachineError::InvalidState("truncated ROM/media hash".into()))?;
        let digest = bytes[cursor..hash_end]
            .try_into()
            .expect("32-byte digest after bounds check");
        cursor = hash_end;
        content.push((slot, digest));
    }
    Ok((content, cursor))
}
