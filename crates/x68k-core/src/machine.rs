//! CPU、バス、映像、音声を統合するX68000マシン。

use m68k::{AddressBus, CpuCore, CpuType, HleHandler, StepResult};
use sha2::{Digest, Sha256};

use crate::bus::Bus;
use crate::media::MediaImage;
use crate::state::{self, CpuSnapshot, StatePayload};
use crate::{
    DriveId, FrameResult, InputEvent, MAX_SCREEN_HEIGHT, MAX_SCREEN_WIDTH, MachineConfig,
    MachineError, MachineModel, MediaFormat, RomKind, VideoOptions,
};

const DEFAULT_WIDTH: u32 = 768;
const DEFAULT_HEIGHT: u32 = 512;
const MIB: usize = 1024 * 1024;

pub struct Machine {
    config: MachineConfig,
    cpu: CpuCore,
    bus: Bus,
    framebuffer: Vec<u16>,
    audio: Vec<f32>,
    audio_remainder: u32,
    cycle_remainder: u32,
    /// 命令境界やメモリwaitでフレーム予算を超えた分。次フレームから差し引く。
    cpu_cycle_debt: u32,
    width: u32,
    height: u32,
    frame_count: u64,
    paused: bool,
    video_options: VideoOptions,
    trace_cpu_traps: bool,
    last_cpu_trap: Option<(u32, u16, &'static str)>,
}

struct TrapRecorder<'a> {
    last: &'a mut Option<(u32, u16, &'static str)>,
}

impl HleHandler for TrapRecorder<'_> {
    fn handle_aline(&mut self, cpu: &mut CpuCore, _bus: &mut dyn AddressBus, opcode: u16) -> bool {
        *self.last = Some((cpu.ppc, opcode, "A-line"));
        false
    }

    fn handle_fline(&mut self, cpu: &mut CpuCore, _bus: &mut dyn AddressBus, opcode: u16) -> bool {
        *self.last = Some((cpu.ppc, opcode, "F-line"));
        false
    }

    fn handle_illegal(
        &mut self,
        cpu: &mut CpuCore,
        _bus: &mut dyn AddressBus,
        opcode: u16,
    ) -> bool {
        *self.last = Some((cpu.ppc, opcode, "illegal"));
        false
    }
}

impl Machine {
    pub fn new(mut config: MachineConfig) -> Result<Self, MachineError> {
        if !(MIB..=12 * MIB).contains(&config.ram_bytes) {
            return Err(MachineError::InvalidRamSize(config.ram_bytes));
        }
        config.ram_bytes = config.ram_bytes.div_ceil(MIB) * MIB;
        if !(8_000..=192_000).contains(&config.sample_rate) {
            config.sample_rate = 48_000;
        }
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(cpu_type(config.model));
        Ok(Self {
            bus: Bus::new(&config, config.ram_bytes),
            config,
            cpu,
            framebuffer: vec![0; (MAX_SCREEN_WIDTH * MAX_SCREEN_HEIGHT) as usize],
            audio: Vec::new(),
            audio_remainder: 0,
            cycle_remainder: 0,
            cpu_cycle_debt: 0,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            frame_count: 0,
            paused: false,
            video_options: VideoOptions::default(),
            trace_cpu_traps: false,
            last_cpu_trap: None,
        })
    }

    pub fn config(&self) -> &MachineConfig {
        &self.config
    }

    pub fn framebuffer(&self) -> &[u16] {
        &self.framebuffer[..(self.width * self.height) as usize]
    }

    pub fn screen_dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// 互換性診断用のCPU位置。第三者CPU型は公開APIへ露出しない。
    pub fn cpu_diagnostics(&self) -> (u32, u16, bool, u32, Option<u32>) {
        let sp = self.cpu.sp();
        let exception_pc = usize::try_from(sp)
            .ok()
            .and_then(|sp| self.bus.ram.get(sp + 2..sp + 6))
            .map(|bytes| u32::from_be_bytes(bytes.try_into().expect("four-byte slice")));
        (
            self.cpu.pc,
            self.cpu.get_sr(),
            self.cpu.stopped != 0,
            sp,
            exception_pc,
        )
    }

    pub fn bus_fault_diagnostics(&self) -> (Option<u32>, Option<u32>, u64) {
        self.bus.fault_diagnostics()
    }

    pub fn fdc_diagnostics(&self) -> (u64, u64, u8, u8, usize) {
        self.bus.fdc_diagnostics()
    }

    pub fn fdc_result_status(&self) -> [u8; 3] {
        self.bus.fdc_result_status()
    }

    pub fn fdc_command_parameters(&self) -> [u8; 8] {
        self.bus.fdc_command_parameters()
    }

    pub fn dma_diagnostics(&self, channel: usize) -> (u8, u8, u8, u8, u16, u32, u32) {
        self.bus.dma_diagnostics(channel)
    }

    /// 実機起動の互換性調査用にRAMの限定windowを返す。
    #[doc(hidden)]
    pub fn ram_diagnostics(&self, address: u32, length: usize) -> Vec<u8> {
        self.bus.ram_diagnostics(address, length)
    }

    #[doc(hidden)]
    pub fn sprite_diagnostics(&self) -> Vec<(u8, u16, u16, u16, u8)> {
        self.bus.sprite_diagnostics()
    }

    #[doc(hidden)]
    pub fn set_cpu_trap_diagnostics(&mut self, enabled: bool) {
        self.trace_cpu_traps = enabled;
        self.last_cpu_trap = None;
    }

    #[doc(hidden)]
    pub fn cpu_trap_diagnostics(&self) -> Option<(u32, u16, &'static str)> {
        self.last_cpu_trap
    }

    pub fn ioc_diagnostics(&self) -> (u8, u8, u8, u8, u32, u64, u64) {
        self.bus.ioc_diagnostics()
    }

    /// 互換性報告へ載せるROM／元媒体のSHA-256（変更overlayは含めない）。
    pub fn content_hashes(&self) -> Vec<(String, [u8; 32])> {
        self.bus.content_hashes()
    }

    pub fn framebuffer_hash(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        for pixel in self.framebuffer() {
            hash.update(pixel.to_be_bytes());
        }
        hash.finalize().into()
    }

    pub fn load_rom(&mut self, kind: RomKind, bytes: &[u8]) -> Result<(), MachineError> {
        match kind {
            RomKind::Ipl if !matches!(bytes.len(), 0x20_000 | 0x40_000) => {
                return Err(MachineError::InvalidRomSize {
                    kind,
                    actual: bytes.len(),
                });
            }
            RomKind::CharacterGenerator if bytes.len() != 0x0c_0000 => {
                return Err(MachineError::InvalidRomSize {
                    kind,
                    actual: bytes.len(),
                });
            }
            RomKind::Scsi if !matches!(bytes.len(), 0x2000 | 0x20_000) => {
                return Err(MachineError::InvalidRomSize {
                    kind,
                    actual: bytes.len(),
                });
            }
            RomKind::Scsi if bytes.len() == 0x2000 => {
                // 8KiB SCSI ROMは先頭ベクタで接続先を識別できる。
                // $FCxxxx はX68030内蔵SCSI (SCSIINROM)、$EAxxxx は
                // X68000/XVI拡張SCSI (SCSIEXROM) なので、機種違いのROMを
                // 誤ってマップするとIPLが別の例外／エラー処理へ進んでしまう。
                let vector = u32::from_be_bytes(bytes[0..4].try_into().expect("8 KiB ROM"));
                let expected = match self.config.model {
                    MachineModel::X68030 => 0x00fc_0000,
                    MachineModel::X68000 | MachineModel::X68000Xvi => 0x00ea_0000,
                };
                let actual = vector & 0x00ff_0000;
                if matches!(actual, 0x00ea_0000 | 0x00fc_0000) && actual != expected {
                    return Err(MachineError::InvalidRomForModel {
                        kind,
                        model: self.config.model,
                        reason: format!(
                            "ROM reset vector targets ${actual:06x}; expected ${expected:06x} for this profile"
                        ),
                    });
                }
            }
            RomKind::Ipl => self.bus.ipl = bytes.to_vec(),
            RomKind::CharacterGenerator => self.bus.cgrom = bytes.to_vec(),
            RomKind::Scsi => self.bus.scsi_rom = bytes.to_vec(),
        }
        if kind == RomKind::Ipl {
            self.reset();
        }
        Ok(())
    }

    pub fn mount_media(
        &mut self,
        drive: DriveId,
        format: MediaFormat,
        bytes: &[u8],
        write_protected: bool,
    ) -> Result<(), MachineError> {
        validate_drive(drive)?;
        if !matches!(
            (drive, format),
            (
                DriveId::Floppy(_),
                MediaFormat::Xdf | MediaFormat::Dim | MediaFormat::D88
            ) | (DriveId::HardDisk(_), MediaFormat::Hdf)
        ) {
            return Err(MachineError::MediaDriveMismatch { drive, format });
        }
        self.bus
            .media
            .insert(drive, MediaImage::parse(format, bytes, write_protected)?);
        self.bus.notify_media_change(drive);
        Ok(())
    }

    pub fn eject_media(&mut self, drive: DriveId) -> Result<Vec<u8>, MachineError> {
        validate_drive(drive)?;
        let media = self
            .bus
            .media
            .remove(&drive)
            .map(|media| media.export())
            .ok_or(MachineError::EmptyDrive(drive))?;
        self.bus.notify_media_change(drive);
        Ok(media)
    }

    pub fn export_media(&self, drive: DriveId) -> Result<Vec<u8>, MachineError> {
        validate_drive(drive)?;
        self.bus
            .media
            .get(&drive)
            .map(MediaImage::export)
            .ok_or(MachineError::EmptyDrive(drive))
    }

    /// デバイス実装・診断用に媒体の1バイトを読み出す。
    pub fn media_read_byte(&self, drive: DriveId, offset: u64) -> Result<u8, MachineError> {
        validate_drive(drive)?;
        self.bus
            .media
            .get(&drive)
            .and_then(|media| media.read(offset))
            .ok_or(MachineError::EmptyDrive(drive))
    }

    /// 元イメージを変更せずコピーオンライト層へ1バイト書き込む。
    pub fn media_write_byte(
        &mut self,
        drive: DriveId,
        offset: u64,
        value: u8,
    ) -> Result<(), MachineError> {
        validate_drive(drive)?;
        let media = self
            .bus
            .media
            .get_mut(&drive)
            .ok_or(MachineError::EmptyDrive(drive))?;
        if media.write_protected {
            return Err(MachineError::WriteProtected(drive));
        }
        media
            .write(offset, value)
            .then_some(())
            .ok_or_else(|| MachineError::InvalidMedia {
                format: media.format,
                reason: "write offset is outside the image".into(),
            })
    }

    pub fn reset(&mut self) {
        self.cpu = CpuCore::new();
        self.cpu.set_cpu_type(cpu_type(self.config.model));
        self.bus.reset();
        if !self.bus.ipl.is_empty() {
            self.cpu.reset(&mut self.bus);
            self.bus.release_reset_overlay();
        }
        self.audio.clear();
        self.audio_remainder = 0;
        self.cycle_remainder = 0;
        self.cpu_cycle_debt = 0;
        self.frame_count = 0;
        self.last_cpu_trap = None;
        let (width, height) = self.bus.screen_dimensions();
        self.width = width.clamp(1, MAX_SCREEN_WIDTH);
        self.height = height.clamp(1, MAX_SCREEN_HEIGHT);
        self.framebuffer.fill(0);
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn input(&mut self, event: InputEvent) {
        self.bus.input(event);
    }

    /// 互換性調査用にCPUを1命令だけ進め、実行前PC/opcodeと実行後SPを返す。
    #[doc(hidden)]
    pub fn step_instruction_diagnostics(&mut self) -> (u32, u16, u32, u16, i32) {
        let pc = self.cpu.pc;
        let opcode = self.bus.read_word(pc);
        let irq = self.bus.pending_irq();
        self.cpu.set_irq(irq);
        self.bus.set_supervisor(self.cpu.is_supervisor());
        let mut ignored_trap = None;
        let mut recorder = TrapRecorder {
            last: &mut ignored_trap,
        };
        let cycles = match self.cpu.step_with_hle_handler(&mut self.bus, &mut recorder) {
            StepResult::Ok { cycles } => cycles.max(1),
            StepResult::Stopped => 1,
            _ => unreachable!("HLE handler returns real exceptions"),
        };
        let wait = self.bus.take_wait_cycles() as i32;
        self.bus.tick((cycles + wait).max(1) as u32);
        (pc, opcode, self.cpu.sp(), self.cpu.get_sr(), cycles + wait)
    }

    pub fn run_frame(&mut self) -> FrameResult {
        // `audio` is a pending FIFO and may contain samples from earlier
        // frames when the host does not drain it immediately.  Keep the
        // per-frame count independent from that FIFO length so callers can
        // use FrameResult for pacing without double-counting queued PCM.
        let mut generated_audio_frames = 0usize;
        if !self.paused {
            let numerator = self.audio_remainder + self.config.sample_rate;
            let audio_frames = (numerator / 60) as usize;
            self.audio_remainder = numerator % 60;
            if !self.bus.ipl.is_empty() {
                let cycle_numerator = self.cycle_remainder + self.config.model.clock_hz();
                let frame_budget = cycle_numerator / 60;
                self.cycle_remainder = cycle_numerator % 60;
                self.bus.begin_audio_frame(frame_budget, audio_frames);
                let mut remaining = frame_budget.saturating_sub(self.cpu_cycle_debt);
                self.cpu_cycle_debt = self.cpu_cycle_debt.saturating_sub(frame_budget);
                while remaining > 0 {
                    // 次の走査線イベントを越えない範囲をまとめて実行する。
                    // 固定の極小sliceより高速で、イベント/IRQ境界の精度も保てる。
                    let slice = remaining.min(self.bus.cycles_until_next_event()).min(4096);
                    let irq = self.bus.pending_irq();
                    self.cpu.set_irq(irq);
                    self.bus.set_supervisor(self.cpu.is_supervisor());
                    // STOP中で、現在のSR maskを超えるIRQが無ければCPUは命令を実行しない。
                    // 周辺クロックだけをイベント境界まで進め、IRQ発生時に通常経路へ戻す。
                    let executed = if self.cpu.stopped != 0 && !self.cpu.check_interrupts() {
                        slice
                    } else {
                        // m68k 0.2.1 の複数命令execute経路は、Human68kの割り込みを伴う
                        // 自己再配置コードでdecode cacheが実メモリと食い違うことがある。
                        // 命令実装そのものは同じCpuCoreへ委譲しつつ、命令境界ごとに
                        // decodeする経路を使って正確性を優先する。
                        let mut executed = 0u32;
                        let mut ignored_trap = None;
                        while executed < slice {
                            self.bus.set_audio_instruction_offset(executed);
                            let irq = self.bus.pending_irq();
                            self.cpu.set_irq(irq);
                            self.bus.set_supervisor(self.cpu.is_supervisor());
                            let last_trap = if self.trace_cpu_traps {
                                &mut self.last_cpu_trap
                            } else {
                                &mut ignored_trap
                            };
                            let mut recorder = TrapRecorder { last: last_trap };
                            match self.cpu.step_with_hle_handler(&mut self.bus, &mut recorder) {
                                StepResult::Ok { cycles } => {
                                    executed = executed.saturating_add(cycles.max(1) as u32);
                                }
                                StepResult::Stopped => {
                                    executed = slice;
                                }
                                _ => unreachable!("HLE handler returns real exceptions"),
                            }
                        }
                        executed
                    };
                    let wait = self.bus.take_wait_cycles();
                    let elapsed = executed.saturating_add(wait).max(1);
                    self.bus.tick(elapsed);
                    // CRTCが可視領域を走査し終えた瞬間のframeだけを公開する。
                    // この後のVBlank/raster IRQでsprite tableが書換え途中になっても、
                    // hostが次に表示するscanoutへ混入させない。
                    if let Some((width, height)) = self.bus.take_scanout(&mut self.framebuffer) {
                        self.width = width.clamp(1, MAX_SCREEN_WIDTH);
                        self.height = height.clamp(1, MAX_SCREEN_HEIGHT);
                    }
                    if elapsed > remaining {
                        self.cpu_cycle_debt = self
                            .cpu_cycle_debt
                            .saturating_add(elapsed.saturating_sub(remaining));
                        remaining = 0;
                    } else {
                        remaining -= elapsed;
                    }
                }
            }
            self.frame_count = self.frame_count.wrapping_add(1);
            if self.bus.ipl.is_empty() {
                let (width, height) = self.bus.screen_dimensions();
                self.width = width.clamp(1, MAX_SCREEN_WIDTH);
                self.height = height.clamp(1, MAX_SCREEN_HEIGHT);
                self.framebuffer[..(self.width * self.height) as usize].fill(0);
            }
            self.bus.generate_audio(audio_frames, &mut self.audio);
            generated_audio_frames = audio_frames;
        }
        FrameResult {
            width: self.width,
            height: self.height,
            audio_frames: generated_audio_frames,
            frame_number: self.frame_count,
        }
    }

    pub fn drain_audio(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.audio)
    }

    pub fn drain_midi(&mut self) -> Vec<u8> {
        self.bus.drain_midi()
    }

    pub fn sram(&self) -> &[u8] {
        &self.bus.sram
    }

    pub fn load_sram(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        if bytes.len() != self.bus.sram.len() {
            return Err(MachineError::InvalidState(format!(
                "SRAM must be {} bytes",
                self.bus.sram.len()
            )));
        }
        self.bus.sram.copy_from_slice(bytes);
        Ok(())
    }

    pub fn video_options(&self) -> VideoOptions {
        self.video_options
    }

    pub fn set_video_options(&mut self, options: VideoOptions) {
        self.video_options = options;
    }

    pub fn save_state(&self) -> Result<Vec<u8>, MachineError> {
        let payload = StatePayload {
            cpu: CpuSnapshot::capture(&self.cpu),
            bus: self.bus.clone(),
            frame_count: self.frame_count,
            audio_remainder: self.audio_remainder,
            cycle_remainder: self.cycle_remainder,
            cpu_cycle_debt: self.cpu_cycle_debt,
            paused: self.paused,
        };
        state::encode(&payload, self.config.model, &self.bus.content_hashes())
    }

    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), MachineError> {
        let mut payload = state::decode(bytes, self.config.model, &self.bus.content_hashes())?;
        if !payload.bus.reattach_immutable(&self.bus) {
            return Err(MachineError::StateMediaMismatch);
        }
        self.cpu = payload.cpu.restore(self.config.model);
        self.bus = payload.bus;
        self.frame_count = payload.frame_count;
        self.audio_remainder = payload.audio_remainder;
        self.cycle_remainder = payload.cycle_remainder;
        self.cpu_cycle_debt = payload.cpu_cycle_debt;
        self.paused = payload.paused;
        self.audio.clear();
        let (width, height) = self.bus.screen_dimensions();
        self.width = width.clamp(1, MAX_SCREEN_WIDTH);
        self.height = height.clamp(1, MAX_SCREEN_HEIGHT);
        self.bus.render_frame(
            &mut self.framebuffer[..(self.width * self.height) as usize],
            self.width,
            self.height,
            self.frame_count,
        );
        Ok(())
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new(MachineConfig::default()).expect("default machine configuration is valid")
    }
}

fn cpu_type(model: MachineModel) -> CpuType {
    match model {
        MachineModel::X68000 | MachineModel::X68000Xvi => CpuType::M68000,
        MachineModel::X68030 => CpuType::M68EC030,
    }
}

fn validate_drive(drive: DriveId) -> Result<(), MachineError> {
    match drive {
        DriveId::Floppy(0..=3) | DriveId::HardDisk(0..=7) => Ok(()),
        _ => Err(MachineError::InvalidDrive(drive)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use m68k::AddressBus;
    use sha2::{Digest, Sha256};

    fn append_move_b_imm_abs(program: &mut Vec<u8>, value: u8, address: u32) {
        program.extend_from_slice(&0x13fcu16.to_be_bytes());
        program.extend_from_slice(&u16::from(value).to_be_bytes());
        program.extend_from_slice(&address.to_be_bytes());
    }

    fn diagnostic_ipl() -> Vec<u8> {
        let mut ipl = vec![0; 0x20_000];
        // IPL末尾側のリセットベクタ: SSP=0x1000, PC=0xfe0010
        ipl[0x10_000..0x10_004].copy_from_slice(&0x0000_1000u32.to_be_bytes());
        ipl[0x10_004..0x10_008].copy_from_slice(&0x00fe_0010u32.to_be_bytes());
        let mut program = Vec::new();
        // 65536色GVRAMを有効化し、先頭画素へ赤を書き込む。
        append_move_b_imm_abs(&mut program, 0x08, 0x00e8_0028);
        append_move_b_imm_abs(&mut program, 0x03, 0x00e8_2401);
        append_move_b_imm_abs(&mut program, 0x01, 0x00e8_2601);
        program.extend_from_slice(&[0x33, 0xfc, 0x07, 0xc0, 0x00, 0xc0, 0x00, 0x00]);
        // YM2151 ch0を両chへ出し、全operatorをkey-onする。
        for (register, value) in [
            (0x20, 0xc7),
            (0x28, 0x4c),
            (0x60, 0x00),
            (0x80, 0x1f),
            (0x08, 0x78),
        ] {
            append_move_b_imm_abs(&mut program, register, 0x00e9_0001);
            append_move_b_imm_abs(&mut program, value, 0x00e9_0003);
        }
        append_move_b_imm_abs(&mut program, 5, 0x00ea_fa03);
        for value in [0x90, 60, 100] {
            append_move_b_imm_abs(&mut program, value, 0x00ea_fa0d);
        }
        for value in [0x06, 0, 0, 0, 1, 3, 1, 0x1b, 0xff] {
            append_move_b_imm_abs(&mut program, value, 0x00e9_4003);
        }
        program.extend_from_slice(&[0x10, 0x39, 0x00, 0xe9, 0x40, 0x03]);
        program.extend_from_slice(&[0x13, 0xc0, 0x00, 0xc0, 0x00, 0x01]);
        program.extend_from_slice(&[0x60, 0xfe]);
        ipl[0x10..0x10 + program.len()].copy_from_slice(&program);
        ipl
    }

    fn mount_diagnostic_xdf(machine: &mut Machine) {
        let mut xdf = vec![0; 77 * 2 * 8 * 1024];
        xdf[0] = 0xc0;
        machine
            .mount_media(DriveId::Floppy(0), MediaFormat::Xdf, &xdf, true)
            .unwrap();
    }

    fn hash_frame(frame: &[u16]) -> String {
        let mut hash = Sha256::new();
        for pixel in frame {
            hash.update(pixel.to_be_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    fn hash_pcm(samples: &[f32]) -> String {
        // 出力APIはf32だが、goldenはホストlibmの末尾差を除外した16bit PCMとする。
        let mut hash = Sha256::new();
        for sample in samples {
            let quantized = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            hash.update(quantized.to_le_bytes());
        }
        format!("{:x}", hash.finalize())
    }

    #[test]
    fn all_models_run_diagnostic_frames() {
        for (model, expected_cpu, expected_clock) in [
            (MachineModel::X68000, CpuType::M68000, 10_000_000),
            (MachineModel::X68000Xvi, CpuType::M68000, 16_000_000),
            (MachineModel::X68030, CpuType::M68EC030, 25_000_000),
        ] {
            let mut machine = Machine::new(MachineConfig {
                model,
                ..MachineConfig::default()
            })
            .unwrap();
            assert_eq!(machine.cpu.cpu_type, expected_cpu);
            assert_eq!(machine.bus.clock_hz, expected_clock);
            mount_diagnostic_xdf(&mut machine);
            machine.load_rom(RomKind::Ipl, &diagnostic_ipl()).unwrap();
            assert_eq!(machine.run_frame().frame_number, 1);
            assert_eq!(machine.framebuffer()[0], 0x07c0);
            assert!(machine.drain_audio().iter().any(|sample| *sample != 0.0));
            assert_eq!(machine.drain_midi(), [0x90, 60, 100]);
        }
    }

    #[test]
    fn eight_kib_scsi_rom_is_checked_against_machine_profile() {
        let mut internal = vec![0; 0x2000];
        internal[..4].copy_from_slice(&0x00fc_0068u32.to_be_bytes());
        let mut external = vec![0; 0x2000];
        external[..4].copy_from_slice(&0x00ea_0066u32.to_be_bytes());

        let mut x68000 = Machine::default();
        assert!(matches!(
            x68000.load_rom(RomKind::Scsi, &internal),
            Err(MachineError::InvalidRomForModel {
                model: MachineModel::X68000,
                ..
            })
        ));
        x68000.load_rom(RomKind::Scsi, &external).unwrap();

        let mut x68030 = Machine::new(MachineConfig {
            model: MachineModel::X68030,
            ..MachineConfig::default()
        })
        .unwrap();
        assert!(matches!(
            x68030.load_rom(RomKind::Scsi, &external),
            Err(MachineError::InvalidRomForModel {
                model: MachineModel::X68030,
                ..
            })
        ));
        x68030.load_rom(RomKind::Scsi, &internal).unwrap();
    }

    #[test]
    fn x68030_stop_advances_devices_without_spinning_cpu() {
        let mut ipl = vec![0; 0x20_000];
        ipl[0x10_000..0x10_004].copy_from_slice(&0x0000_1000u32.to_be_bytes());
        ipl[0x10_004..0x10_008].copy_from_slice(&0x00fe_0010u32.to_be_bytes());
        // stop #$2700 / bra.s -6
        ipl[0x10..0x16].copy_from_slice(&[0x4e, 0x72, 0x27, 0x00, 0x60, 0xfa]);
        let mut machine = Machine::new(MachineConfig {
            model: MachineModel::X68030,
            ..MachineConfig::default()
        })
        .unwrap();
        machine.load_rom(RomKind::Ipl, &ipl).unwrap();
        machine.run_frame();
        assert_ne!(machine.cpu.stopped, 0);
        assert_eq!(machine.frame_count(), 1);
    }

    #[test]
    fn save_state_round_trip_is_deterministic() {
        let mut machine = Machine::default();
        machine.run_frame();
        let state = machine.save_state().unwrap();
        let expected = machine.framebuffer().to_vec();
        machine.run_frame();
        machine.load_state(&state).unwrap();
        assert_eq!(machine.frame_count(), 1);
        assert_eq!(machine.framebuffer(), expected);
    }

    #[test]
    fn media_formats_are_restricted_to_their_physical_drive_kind() {
        let mut machine = Machine::default();
        assert!(matches!(
            machine.mount_media(
                DriveId::HardDisk(0),
                MediaFormat::Xdf,
                &vec![0; 77 * 2 * 8 * 1024],
                true,
            ),
            Err(MachineError::MediaDriveMismatch { .. })
        ));
        assert!(matches!(
            machine.mount_media(DriveId::Floppy(0), MediaFormat::Hdf, &vec![0; 1024], true,),
            Err(MachineError::MediaDriveMismatch { .. })
        ));
    }

    #[test]
    fn no_rom_uses_black_frame_instead_of_phase_zero_test_pattern() {
        let mut machine = Machine::default();
        let first = machine.run_frame();
        assert_eq!(first.audio_frames, 800);
        let second = machine.run_frame();
        assert_eq!(second.audio_frames, 800);
        assert!(machine.framebuffer().iter().all(|pixel| *pixel == 0));
        assert_eq!(
            machine.drain_audio().len(),
            2 * (first.audio_frames + second.audio_frames)
        );

        machine.set_paused(true);
        let paused = machine.run_frame();
        assert_eq!(paused.audio_frames, 0);
        assert_eq!(paused.frame_number, second.frame_number);
    }

    #[test]
    fn save_state_manifest_lists_hashes_without_embedding_immutable_assets() {
        let mut machine = Machine::default();
        let mut ipl = diagnostic_ipl();
        for (index, byte) in ipl.iter_mut().enumerate() {
            *byte ^= index.wrapping_mul(73) as u8;
        }
        // reset vectorだけは診断ROMとして有効な値へ戻す。
        ipl[0x10_000..0x10_004].copy_from_slice(&0x0000_1000u32.to_be_bytes());
        ipl[0x10_004..0x10_008].copy_from_slice(&0x00fe_0010u32.to_be_bytes());
        machine.load_rom(RomKind::Ipl, &ipl).unwrap();
        let xdf = (0usize..77 * 2 * 8 * 1024)
            .map(|index| index.wrapping_mul(29) as u8)
            .collect::<Vec<_>>();
        machine
            .mount_media(DriveId::Floppy(0), MediaFormat::Xdf, &xdf, false)
            .unwrap();

        let state = machine.save_state().unwrap();
        let (manifest, _) = state::decode_manifest(&state).unwrap();
        assert_eq!(
            manifest
                .iter()
                .map(|(slot, _)| slot.as_str())
                .collect::<Vec<_>>(),
            ["rom:ipl", "fdd:0"]
        );
        // 1.3MiB超の元媒体を含めず、RAM/VRAM/デバイス/overlayだけを圧縮する。
        assert!(
            state.len() < 256 * 1024,
            "state unexpectedly embeds immutable media"
        );
    }

    #[test]
    fn synthetic_ipl_executes_and_writes_graphic_vram() {
        let mut machine = Machine::default();
        mount_diagnostic_xdf(&mut machine);
        let ipl = diagnostic_ipl();
        machine.load_rom(RomKind::Ipl, &ipl).unwrap();
        machine.run_frame();
        assert_eq!(machine.framebuffer()[0], 0x07c0);
    }

    #[test]
    fn diagnostic_golden_and_state_reexecution_are_deterministic() {
        let mut machine = Machine::default();
        mount_diagnostic_xdf(&mut machine);
        machine.load_rom(RomKind::Ipl, &diagnostic_ipl()).unwrap();
        machine.run_frame();
        machine.drain_audio();
        let state = machine.save_state().unwrap();

        machine.run_frame();
        let first_frame_hash = hash_frame(machine.framebuffer());
        let first_pcm_hash = hash_pcm(&machine.drain_audio());

        machine.load_state(&state).unwrap();
        machine.run_frame();
        assert_eq!(hash_frame(machine.framebuffer()), first_frame_hash);
        assert_eq!(hash_pcm(&machine.drain_audio()), first_pcm_hash);

        assert_eq!(
            first_frame_hash,
            "15dc6f5063bf79953f46d6f576e6a06a4494a7568c7469c8f9349a4f3276d43c"
        );
        assert_eq!(
            first_pcm_hash,
            "29dab4aba7bbd914a5387712d98b63f098c9ddb49f10f67fa9d97486a1aaa8c3"
        );
    }

    #[test]
    fn save_state_restores_copy_on_write_overlay() {
        let mut machine = Machine::default();
        machine
            .mount_media(
                DriveId::Floppy(0),
                MediaFormat::Xdf,
                &vec![0; 77 * 2 * 8 * 1024],
                false,
            )
            .unwrap();
        machine.media_write_byte(DriveId::Floppy(0), 42, 7).unwrap();
        let state = machine.save_state().unwrap();
        machine.media_write_byte(DriveId::Floppy(0), 42, 9).unwrap();
        machine.load_state(&state).unwrap();
        assert_eq!(machine.media_read_byte(DriveId::Floppy(0), 42).unwrap(), 7);
    }

    #[test]
    fn save_state_rejects_corruption_model_and_media_mismatch() {
        let source = Machine::default();
        let state = source.save_state().unwrap();

        let mut corrupt = state.clone();
        *corrupt.last_mut().unwrap() ^= 0x80;
        assert!(matches!(
            Machine::default().load_state(&corrupt),
            Err(MachineError::InvalidState(_))
        ));

        // The LZ4 size prefix is untrusted input.  Recompute the outer CRC so
        // this specifically exercises the allocation guard rather than the
        // generic corruption check.
        let (_, payload_offset) = state::decode_manifest(&state).unwrap();
        let mut oversized = state.clone();
        oversized[payload_offset + 4..payload_offset + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        let checksum = crc32fast::hash(&oversized[payload_offset + 4..]);
        oversized[payload_offset..payload_offset + 4].copy_from_slice(&checksum.to_le_bytes());
        assert!(matches!(
            Machine::default().load_state(&oversized),
            Err(MachineError::InvalidState(_))
        ));

        let mut other_model = Machine::new(MachineConfig {
            model: MachineModel::X68030,
            ..MachineConfig::default()
        })
        .unwrap();
        assert!(matches!(
            other_model.load_state(&state),
            Err(MachineError::StateModelMismatch { .. })
        ));

        let mut other_media = Machine::default();
        other_media
            .mount_media(
                DriveId::Floppy(0),
                MediaFormat::Xdf,
                &vec![0; 77 * 2 * 8 * 1024],
                true,
            )
            .unwrap();
        assert!(matches!(
            other_media.load_state(&state),
            Err(MachineError::StateMediaMismatch)
        ));
    }

    #[test]
    fn load_state_immediately_restores_crtc_dimensions() {
        let mut machine = Machine::default();
        machine.bus.write_byte(0xe8_0004, 0);
        machine.bus.write_byte(0xe8_0005, 10);
        machine.bus.write_byte(0xe8_0006, 0);
        machine.bus.write_byte(0xe8_0007, 90);
        machine.run_frame();
        assert_eq!(machine.screen_dimensions().0, 640);
        let state = machine.save_state().unwrap();

        machine.bus.write_byte(0xe8_0007, 50);
        machine.run_frame();
        assert_eq!(machine.screen_dimensions().0, 320);
        machine.load_state(&state).unwrap();
        assert_eq!(machine.screen_dimensions().0, 640);
        assert_eq!(machine.framebuffer().len(), 640 * machine.height as usize);
    }
}
