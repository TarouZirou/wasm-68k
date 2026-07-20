//! YM2151 (OPM) と MSM6258 ADPCM の決定論的ステレオ音源。
//!
//! レジスタ配置、タイマ接続、ADPCM差分表は PX68k の `fmgen` ラッパと
//! `x68k/adpcm.c` を比較資料としている。FM演算部はRustで独立実装している。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use super::opm::Ym2151;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Msm6258 {
    playing: bool,
    predictor: i32,
    step: i32,
    decoded: VecDeque<i16>,
    current: i16,
    phase: f64,
    clock_select: u8,
    pan: u8,
}

impl Default for Msm6258 {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            playing: false,
            predictor: 0,
            step: 0,
            decoded: VecDeque::new(),
            current: 0,
            phase: 0.0,
            clock_select: 0,
            pan: 0x0b,
        }
    }
}

impl Msm6258 {
    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    fn write(&mut self, offset: u32, value: u8) {
        match offset {
            1 if value & 1 != 0 => self.playing = false,
            1 if value & 2 != 0 => {
                self.playing = true;
                self.predictor = 0;
                self.step = 0;
                self.decoded.clear();
            }
            3 if self.playing => {
                self.decode(value & 0x0f);
                self.decode(value >> 4);
            }
            _ => {}
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    fn read(&self, offset: u32) -> u8 {
        if offset == 1 {
            if self.playing { 0xc0 } else { 0x40 }
        } else {
            0
        }
    }

    /// 入力を解析し、後続処理で利用できる正規化済みの結果を返す。
    fn decode(&mut self, nibble: u8) {
        const INDEX_SHIFT: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
        const STEP_SIZE: [i32; 49] = [
            16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
            118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544,
            598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552,
        ];
        let magnitude = STEP_SIZE[self.step as usize];
        let mut difference = magnitude / 8;
        if nibble & 1 != 0 {
            difference += magnitude / 4;
        }
        if nibble & 2 != 0 {
            difference += magnitude / 2;
        }
        if nibble & 4 != 0 {
            difference += magnitude;
        }
        self.predictor += if nibble & 8 != 0 {
            -difference
        } else {
            difference
        };
        self.predictor = self.predictor.clamp(-2048, 2047);
        self.step = (self.step + INDEX_SHIFT[usize::from(nibble & 7)]).clamp(0, 48);
        self.decoded.push_back((self.predictor * 16) as i16);
    }

    /// MSM6258 ADPCMの現在信号を1サンプル進めて返す。
    fn sample(&mut self, sample_rate: u32) -> (f32, f32) {
        const RATES: [f64; 8] = [
            7812.5, 10416.667, 15625.0, 10416.667, 3906.25, 5208.333, 7812.5, 5208.333,
        ];
        if self.playing {
            self.phase += RATES[usize::from(self.clock_select & 7)] / f64::from(sample_rate);
            while self.phase >= 1.0 {
                self.phase -= 1.0;
                if let Some(sample) = self.decoded.pop_front() {
                    self.current = sample;
                }
            }
        }
        let sample = f32::from(self.current) / 32768.0;
        (
            if self.pan & 2 == 0 { sample } else { 0.0 },
            if self.pan & 1 == 0 { sample } else { 0.0 },
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct AudioSystem {
    ym2151: Ym2151,
    msm6258: Msm6258,
    /// CPU frame内のregister write時刻に合わせてPCMを分割生成するhost側状態。
    /// save stateはframe境界でだけ作るためpayloadには含めない。
    #[serde(skip, default)]
    frame: AudioFrame,
}

#[derive(Debug, Clone, Default)]
struct AudioFrame {
    active: bool,
    cycle_budget: u32,
    sample_frames: usize,
    sample_rate: u32,
    elapsed_cycles: u32,
    instruction_offset: u32,
    generated_frames: usize,
    output: Vec<f32>,
}

impl AudioSystem {
    /// 現在の状態や結果を利用者向けの診断情報として提示する。
    pub(crate) fn diagnostics(&self) -> (u64, u64, u8, u16) {
        self.ym2151.diagnostics()
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    pub(crate) fn read_ym(&self, offset: u32) -> u8 {
        self.ym2151.read(offset)
    }

    /// YM2151タイマーIRQ出力が現在アサート中かを返す。
    pub(crate) fn ym_irq_asserted(&self) -> bool {
        self.ym2151.irq_asserted()
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    pub(crate) fn write_ym(&mut self, offset: u32, value: u8) {
        // data registerの効果をframe末尾のPCM全体へ遡及させない。address latch
        // 自体は無音なので、実際のregister write直前までを旧状態で生成する。
        if offset & 3 == 3 {
            self.render_to_cursor();
        }
        self.ym2151.write(offset, value);
        if offset & 3 == 3 && self.ym2151.address == 0x1b {
            self.msm6258.clock_select = (value >> 5) & 4 | (self.msm6258.pan >> 2) & 3;
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    pub(crate) fn read_adpcm(&self, offset: u32) -> u8 {
        self.msm6258.read(offset)
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    pub(crate) fn write_adpcm(&mut self, offset: u32, value: u8) {
        self.render_to_cursor();
        self.msm6258.write(offset, value);
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_pan(&mut self, value: u8) {
        self.render_to_cursor();
        self.msm6258.pan = value & 0x0f;
        self.msm6258.clock_select = (self.msm6258.clock_select & 4) | ((value >> 2) & 3);
    }

    /// 経過CPUクロックをデバイス固有クロックへ変換し、タイマーと転送状態を進める。
    pub(crate) fn tick(&mut self, cycles: u32, cpu_clock: u32) -> bool {
        let irq = self.ym2151.tick(cycles, cpu_clock);
        if self.frame.active {
            self.frame.elapsed_cycles = self.frame.elapsed_cycles.saturating_add(cycles);
            self.frame.instruction_offset = 0;
        }
        irq
    }

    /// フレーム単位の音声サンプル収集を初期化する。
    pub(crate) fn begin_frame(
        &mut self,
        cycle_budget: u32,
        sample_frames: usize,
        sample_rate: u32,
    ) {
        self.frame.active = true;
        self.frame.cycle_budget = cycle_budget.max(1);
        self.frame.sample_frames = sample_frames;
        self.frame.sample_rate = sample_rate;
        self.frame.elapsed_cycles = 0;
        self.frame.instruction_offset = 0;
        self.frame.generated_frames = 0;
        self.frame.output.clear();
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_instruction_offset(&mut self, cycles: u32) {
        if self.frame.active {
            self.frame.instruction_offset = cycles;
        }
    }

    /// 指定サンプル数のFM・ADPCMステレオPCMを生成する。
    pub(crate) fn generate(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        output.reserve(frames * 2);
        let start = output.len();
        self.ym2151.generate(frames, sample_rate, output);
        for frame in 0..frames {
            let adpcm = self.msm6258.sample(sample_rate);
            let offset = start + frame * 2;
            output[offset] = (output[offset] + adpcm.0 * 0.5).clamp(-1.0, 1.0);
            output[offset + 1] = (output[offset + 1] + adpcm.1 * 0.5).clamp(-1.0, 1.0);
        }
    }

    /// フレーム末端まで音声を生成し、ステレオPCMブロックを確定する。
    pub(crate) fn finish_frame(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        if !self.frame.active {
            self.generate(frames, sample_rate, output);
            return;
        }
        self.render_to_frame(frames);
        output.append(&mut self.frame.output);
        self.frame = AudioFrame::default();
    }

    /// 現在の映像状態を出力先へ描画し、表示に必要な変換を適用する。
    fn render_to_cursor(&mut self) {
        if !self.frame.active {
            return;
        }
        let cycles = self
            .frame
            .elapsed_cycles
            .saturating_add(self.frame.instruction_offset)
            .min(self.frame.cycle_budget);
        let target = (u64::from(cycles) * self.frame.sample_frames as u64
            / u64::from(self.frame.cycle_budget)) as usize;
        self.render_to_frame(target);
    }

    /// 現在の映像状態を出力先へ描画し、表示に必要な変換を適用する。
    fn render_to_frame(&mut self, target: usize) {
        let target = target.min(self.frame.sample_frames);
        let count = target.saturating_sub(self.frame.generated_frames);
        if count == 0 {
            return;
        }
        let mut output = std::mem::take(&mut self.frame.output);
        self.generate(count, self.frame.sample_rate, &mut output);
        self.frame.output = output;
        self.frame.generated_frames = target;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_ym(audio: &mut AudioSystem, register: u8, value: u8) {
        audio.write_ym(1, register);
        audio.write_ym(3, value);
    }

    /// YM2151のキーオン値を対象チャンネルの各オペレータへ反映する。
    fn key_on_channel(audio: &mut AudioSystem, channel: u8, algorithm: u8) {
        write_ym(audio, 0x20 + channel, 0xc0 | algorithm);
        write_ym(audio, 0x28 + channel, 0x4c);
        for slot in 0..4 {
            let offset = slot * 8 + channel;
            write_ym(audio, 0x40 + offset, 1);
            write_ym(audio, 0x60 + offset, 0);
            write_ym(audio, 0x80 + offset, 0x1f);
        }
        write_ym(audio, 0x08, 0x78 | channel);
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    fn configure_single_carrier(audio: &mut AudioSystem, control: u8) {
        write_ym(audio, 0x20, control | 7);
        write_ym(audio, 0x28, 0x4c);
        write_ym(audio, 0x40, 1);
        write_ym(audio, 0x60, 0);
        write_ym(audio, 0x80, 0x1f);
    }

    #[test]
    /// `ym2151_key_on_generates_stereo_pcm` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_key_on_generates_stereo_pcm() {
        let mut audio = AudioSystem::default();
        key_on_channel(&mut audio, 0, 7);
        let mut samples = Vec::new();
        audio.generate(512, 48_000, &mut samples);
        assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
        assert_eq!(samples.len(), 1024);
    }

    #[test]
    /// `ym2151_rl_bits_route_left_and_right_in_chip_order` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_rl_bits_route_left_and_right_in_chip_order() {
        let mut audio = AudioSystem::default();
        configure_single_carrier(&mut audio, 0x80);
        write_ym(&mut audio, 0x08, 0x08);
        let mut samples = Vec::new();
        audio.generate(512, 48_000, &mut samples);
        assert!(samples.chunks_exact(2).any(|frame| frame[0].abs() > 0.0001));
        assert!(samples.chunks_exact(2).all(|frame| frame[1] == 0.0));
    }

    #[test]
    /// `ym2151_zero_attack_rate_does_not_open_envelope` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_zero_attack_rate_does_not_open_envelope() {
        let mut audio = AudioSystem::default();
        write_ym(&mut audio, 0x20, 0xc7);
        write_ym(&mut audio, 0x28, 0x4c);
        write_ym(&mut audio, 0x40, 1);
        write_ym(&mut audio, 0x60, 0);
        // AR=0は停止rateであり、key-onだけではenvelopeが立ち上がらない。
        write_ym(&mut audio, 0x08, 0x08);
        let mut samples = Vec::new();
        audio.generate(512, 48_000, &mut samples);
        assert!(samples.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    /// `frame_timeline_keeps_short_key_event_at_its_cycle_position` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn frame_timeline_keeps_short_key_event_at_its_cycle_position() {
        let mut audio = AudioSystem::default();
        configure_single_carrier(&mut audio, 0xc0);
        audio.begin_frame(1_000, 100, 48_000);
        audio.set_instruction_offset(250);
        write_ym(&mut audio, 0x08, 0x08);
        audio.set_instruction_offset(750);
        write_ym(&mut audio, 0x08, 0x00);

        let mut samples = Vec::new();
        audio.finish_frame(100, 48_000, &mut samples);
        assert_eq!(samples.len(), 200);
        assert!(samples[..50].iter().all(|sample| *sample == 0.0));
        assert!(samples[50..150].iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    /// `redistributable_ym_register_trace_is_deterministic` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn redistributable_ym_register_trace_is_deterministic() {
        #[derive(Deserialize)]
        struct RegisterWrite {
            register: u8,
            value: u8,
        }

        let trace: Vec<RegisterWrite> = serde_json::from_str(include_str!(
            "../../tests/fixtures/audio/ym2151_register_trace.json"
        ))
        .expect("valid YM2151 trace fixture");
        let mut audio = AudioSystem::default();
        for write in trace {
            write_ym(&mut audio, write.register, write.value);
        }
        let mut samples = Vec::new();
        audio.generate(256, 48_000, &mut samples);
        assert_eq!(samples.len(), 512);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    /// `ym2151_native_trace_matches_ymfm_reference` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_native_trace_matches_ymfm_reference() {
        let mut audio = AudioSystem::default();
        for (register, value) in [(32, 199), (40, 76), (64, 1), (96, 0), (128, 31), (8, 120)] {
            write_ym(&mut audio, register, value);
        }
        let mut samples = Vec::new();
        audio.generate(32, 62_500, &mut samples);
        let pcm = samples
            .iter()
            .map(|sample| (sample * 32768.0) as i16)
            .collect::<Vec<_>>();
        assert_eq!(
            pcm,
            [
                426, 426, 874, 874, 1272, 1272, 1716, 1716, 2104, 2104, 2536, 2536, 2920, 2920,
                3328, 3328, 3696, 3696, 4096, 4096, 4432, 4432, 4800, 4800, 5120, 5120, 5472, 5472,
                5792, 5792, 6064, 6064, 6368, 6368, 6608, 6608, 6864, 6864, 7072, 7072, 7280, 7280,
                7440, 7440, 7632, 7632, 7760, 7760, 7888, 7888, 7984, 7984, 8048, 8048, 8112, 8112,
                8144, 8144, 8160, 8160, 8144, 8144, 8112, 8112,
            ]
        );
    }

    #[test]
    /// `ym2151_complex_native_trace_matches_ymfm_hash` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_complex_native_trace_matches_ymfm_hash() {
        use sha2::{Digest, Sha256};

        let mut audio = AudioSystem::default();
        for (register, value) in [
            (0x20, 0xfd),
            (0x28, 0x48),
            (0x30, 0x80),
            (0x38, 0x73),
            (0x18, 0xc5),
            (0x19, 0x40),
            (0x19, 0xc0),
            (0x1b, 0x02),
        ] {
            write_ym(&mut audio, register, value);
        }
        let offsets = [0, 8, 16, 24];
        let values = [
            [0x11, 0x22, 0x43, 0x74],
            [0, 10, 15, 20],
            [0x1f, 0x5c, 0x98, 0xd4],
            [0x9a, 0x8e, 0x87, 0x83],
            [0x0c, 0x48, 0x84, 0xc2],
            [0x27, 0x49, 0x6b, 0x8d],
        ];
        for slot in 0..4 {
            for (base, row) in [0x40, 0x60, 0x80, 0xa0, 0xc0, 0xe0].into_iter().zip(values) {
                write_ym(&mut audio, base + offsets[slot], row[slot]);
            }
        }
        write_ym(&mut audio, 0x08, 0x78);
        let mut samples = Vec::new();
        audio.generate(4096, 62_500, &mut samples);
        let mut hash = Sha256::new();
        for sample in samples {
            hash.update(((sample * 32768.0) as i16).to_le_bytes());
        }
        assert_eq!(
            format!("{:x}", hash.finalize()),
            "3da75d9aae38d631004d0272f6fc0d6d9681502aee6b2dc8c260766ae67ca58d"
        );
    }

    #[test]
    /// `msm6258_decodes_nibbles_and_obeys_pan` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn msm6258_decodes_nibbles_and_obeys_pan() {
        let mut audio = AudioSystem::default();
        audio.set_pan(0);
        audio.write_adpcm(1, 2);
        for _ in 0..32 {
            audio.write_adpcm(3, 0x77);
        }
        let mut samples = Vec::new();
        audio.generate(512, 48_000, &mut samples);
        assert!(samples.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    /// `msm6258_uses_the_chip_integer_difference_table` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn msm6258_uses_the_chip_integer_difference_table() {
        let mut chip = Msm6258::default();
        chip.decode(0x07);
        assert_eq!((chip.predictor, chip.step), (30, 8));
        chip.decode(0x0f);
        assert_eq!((chip.predictor, chip.step), (-33, 16));
    }

    #[test]
    /// `ym2151_all_algorithms_stay_finite_under_feedback` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_all_algorithms_stay_finite_under_feedback() {
        for algorithm in 0..8 {
            let mut audio = AudioSystem::default();
            key_on_channel(&mut audio, 0, algorithm);
            write_ym(&mut audio, 0x20, 0xf8 | algorithm);
            let mut samples = Vec::new();
            audio.generate(4096, 48_000, &mut samples);
            assert!(samples.iter().all(|sample| sample.is_finite()));
            assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
        }
    }

    #[test]
    /// `ym2151_lfo_noise_detune_and_timer_registers_affect_output` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_lfo_noise_detune_and_timer_registers_affect_output() {
        let mut dry = AudioSystem::default();
        key_on_channel(&mut dry, 7, 7);
        let mut modulated = dry.clone();
        write_ym(&mut modulated, 0x18, 0xff);
        write_ym(&mut modulated, 0x19, 0x7f); // AMD
        write_ym(&mut modulated, 0x19, 0xff); // PMD
        write_ym(&mut modulated, 0x3f, 0x73); // channel 7 PMS/AMS
        write_ym(&mut modulated, 0x0f, 0x9f); // noise enable/rate
        write_ym(&mut modulated, 0x5f, 0x71); // operator 4 DT1/MUL
        write_ym(&mut modulated, 0xdf, 0xc0); // operator 4 DT2

        let mut dry_samples = Vec::new();
        let mut modulated_samples = Vec::new();
        dry.generate(2048, 48_000, &mut dry_samples);
        modulated.generate(2048, 48_000, &mut modulated_samples);
        assert_ne!(dry_samples, modulated_samples);
        assert!(modulated_samples.iter().all(|sample| sample.is_finite()));

        let mut timer = AudioSystem::default();
        write_ym(&mut timer, 0x10, 0xff);
        write_ym(&mut timer, 0x11, 3);
        write_ym(&mut timer, 0x14, 0x05); // timer A start + IRQ enable
        assert!(timer.tick(160, 10_000_000));
        assert_eq!(timer.read_ym(3) & 1, 1);
        write_ym(&mut timer, 0x14, 0x10); // clear timer A status
        assert_eq!(timer.read_ym(3) & 1, 0);
    }

    #[test]
    /// `ym2151_operator_register_order_matches_opm_wiring` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_operator_register_order_matches_opm_wiring() {
        let mut chip = Ym2151::default();
        chip.write(1, 0x68);
        chip.write(3, 20); // register bank M2
        chip.write(1, 0x70);
        chip.write(3, 40); // register bank C1
        assert_eq!(chip.operator_total_level(0, 1), 40 << 3);
        assert_eq!(chip.operator_total_level(0, 2), 20 << 3);
    }

    #[test]
    /// `ym2151_key_on_edge_resets_phase_without_retriggering_held_key` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_key_on_edge_resets_phase_without_retriggering_held_key() {
        let mut audio = AudioSystem::default();
        key_on_channel(&mut audio, 0, 7);
        let mut samples = Vec::new();
        audio.generate(16, 48_000, &mut samples);
        let running_phase = audio.ym2151.operator_phase(0, 0);
        assert!(running_phase > 0);

        write_ym(&mut audio, 0x08, 0x78);
        assert_eq!(audio.ym2151.operator_phase(0, 0), running_phase);
        write_ym(&mut audio, 0x08, 0x00);
        write_ym(&mut audio, 0x08, 0x78);
        assert_eq!(audio.ym2151.operator_phase(0, 0), 0);
    }

    #[test]
    /// `ym2151_pcm_does_not_depend_on_generate_block_size` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_pcm_does_not_depend_on_generate_block_size() {
        let mut whole = AudioSystem::default();
        key_on_channel(&mut whole, 0, 7);
        let mut split = whole.clone();
        let mut whole_samples = Vec::new();
        let mut split_samples = Vec::new();
        whole.generate(512, 48_000, &mut whole_samples);
        split.generate(256, 48_000, &mut split_samples);
        split.generate(256, 48_000, &mut split_samples);
        assert_eq!(whole_samples, split_samples);
    }

    #[test]
    #[ignore = "manual real-time performance probe"]
    /// `ym2151_ten_seconds_realtime_performance_probe` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn ym2151_ten_seconds_realtime_performance_probe() {
        let mut audio = AudioSystem::default();
        for channel in 0..8 {
            key_on_channel(&mut audio, channel, channel);
        }
        let started = std::time::Instant::now();
        let mut samples = Vec::new();
        audio.generate(480_000, 48_000, &mut samples);
        let elapsed = started.elapsed();
        eprintln!(
            "generated 10 seconds of 8-channel FM in {:.3}s ({:.1}x realtime)",
            elapsed.as_secs_f64(),
            10.0 / elapsed.as_secs_f64()
        );
        assert_eq!(samples.len(), 960_000);
    }
}
