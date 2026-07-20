//! YM2151 (OPM) の整数テーブル駆動FM合成部。
//!
//! レジスタ配線、EGクロック、アルゴリズムと変調量は BSD-3-Clause の
//! Aaron Giles `ymfm` OPM 実装を比較資料として、Rustで独立実装している。

use serde::{Deserialize, Serialize};

use super::opm_tables::{ATTENUATION_VOLUME, PHASE_STEP, SINE_ATTENUATION};

const OPM_CLOCK: u32 = 4_000_000;
pub(super) const OPM_SAMPLE_RATE: u32 = OPM_CLOCK / 64;
const EG_QUIET: u16 = 0x380;

const EG_ATTACK: u8 = 0;
const EG_DECAY: u8 = 1;
const EG_SUSTAIN: u8 = 2;
const EG_RELEASE: u8 = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Operator {
    /// 10.10形式。上位側の10bitが1周期の位相となる。
    phase: u32,
    /// 4.6形式の減衰量。0が最大音量、0x3ffが無音。
    envelope: u16,
    envelope_state: u8,
    key_on: bool,
}

impl Default for Operator {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            phase: 0,
            envelope: 0x3ff,
            envelope_state: EG_RELEASE,
            key_on: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Channel {
    /// OPMの自然な接続順 M1, C1, M2, C2。
    operators: [Operator; 4],
    feedback: [i16; 2],
    feedback_input: i16,
}

#[derive(Debug, Clone, Copy, Default)]
struct OperatorParameters {
    block_frequency: u32,
    detune: i32,
    detune2: u8,
    multiple: u32,
    phase_step: u32,
    total_level: u16,
    sustain_level: u16,
    envelope_rates: [u8; 4],
    amplitude_modulation: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChannelParameters {
    control: u8,
    pm_sensitivity: u8,
    am_sensitivity: u8,
    operators: [OperatorParameters; 4],
}

#[derive(Debug, Clone)]
struct RenderParameters {
    channels: [ChannelParameters; 8],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Ym2151 {
    registers: Vec<u8>,
    pub(super) address: u8,
    channels: [Channel; 8],
    envelope_counter: u32,
    lfo_counter: u32,
    noise_lfsr: u32,
    noise_counter: u8,
    noise_state: u8,
    lfo_amplitude: u8,
    /// 動作中timerの満了までのOPM master clock数。load解除中は0。
    timer_a_counter: u64,
    timer_b_counter: u64,
    timer_fraction: u64,
    status: u8,
    /// 出力rateへ変換するための有理数位相と、1sample遅延の線形補間状態。
    resample_accumulator: u32,
    resample_previous: [i16; 2],
    resample_current: [i16; 2],
    resample_started: bool,
    #[serde(skip, default)]
    diagnostic_register_writes: u64,
    #[serde(skip, default)]
    diagnostic_key_ons: u64,
    diagnostic_peak: u16,
}

impl Default for Ym2151 {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        let mut registers = vec![0; 256];
        // 実チップreset値と同様、全channelの左右出力を有効にする。
        registers[0x20..0x28].fill(0xc0);
        Self {
            registers,
            address: 0,
            channels: [Channel::default(); 8],
            envelope_counter: 0,
            lfo_counter: 0,
            noise_lfsr: 1,
            noise_counter: 0,
            noise_state: 0,
            lfo_amplitude: 0,
            timer_a_counter: 0,
            timer_b_counter: 0,
            timer_fraction: 0,
            status: 0,
            resample_accumulator: 0,
            resample_previous: [0; 2],
            resample_current: [0; 2],
            resample_started: false,
            diagnostic_register_writes: 0,
            diagnostic_key_ons: 0,
            diagnostic_peak: 0,
        }
    }
}

impl Ym2151 {
    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(super) fn write(&mut self, offset: u32, value: u8) {
        match offset & 3 {
            1 => self.address = value,
            3 => self.write_register(self.address, value),
            _ => {}
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(super) fn read(&self, offset: u32) -> u8 {
        // X68000のOPMは奇数アドレスへ接続されている。実ソフトには
        // $E90001と$E90003の双方でbusy/statusを読むものがあるため、
        // どちらのreadも同じstatusを返す。
        if offset & 1 == 1 { self.status } else { 0 }
    }

    /// `irq_asserted` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(super) fn irq_asserted(&self) -> bool {
        let control = self.registers[0x14];
        self.status & ((control >> 2) & 3) != 0
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_register(&mut self, register: u8, value: u8) {
        self.diagnostic_register_writes = self.diagnostic_register_writes.saturating_add(1);
        let register_index = usize::from(register);
        let previous = self.registers[register_index];
        if register == 0x19 {
            // AMD/PMDは同じaddressへ書く。内部ではPMDを空きregisterへ分離する。
            let target = 0x19 + usize::from(value >> 7);
            self.registers[target] = value & 0x7f;
        } else if register != 0x1a {
            self.registers[register_index] = value;
        }

        match register {
            0x08 => self.write_key_on(value),
            0x14 => {
                if value & 0x10 != 0 {
                    self.status &= !1;
                }
                if value & 0x20 != 0 {
                    self.status &= !2;
                }
                // load bitの立上りでtimerをreloadする。動作中のperiod register
                // 書換えは現在の周期を変えず、次の満了時から反映される。
                if value & 1 == 0 {
                    self.timer_a_counter = 0;
                } else if previous & 1 == 0 {
                    self.timer_a_counter = self.timer_a_period();
                }
                if value & 2 == 0 {
                    self.timer_b_counter = 0;
                } else if previous & 2 == 0 {
                    self.timer_b_counter = self.timer_b_period();
                }
            }
            _ => {}
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_key_on(&mut self, value: u8) {
        let channel_index = usize::from(value & 7);
        if channel_index >= self.channels.len() {
            return;
        }
        let parameters = self.render_parameters();
        for slot in 0..4 {
            let keyed = value & (1 << (slot + 3)) != 0;
            let operator = &mut self.channels[channel_index].operators[slot];
            if keyed == operator.key_on {
                continue;
            }
            operator.key_on = keyed;
            if keyed {
                self.diagnostic_key_ons = self.diagnostic_key_ons.saturating_add(1);
                operator.phase = 0;
                operator.envelope_state = EG_ATTACK;
                if parameters.channels[channel_index].operators[slot].envelope_rates
                    [usize::from(EG_ATTACK)]
                    >= 62
                {
                    operator.envelope = 0;
                }
            } else {
                operator.envelope_state = EG_RELEASE;
            }
        }
    }

    /// 経過CPUクロックをデバイス固有クロックへ変換し、タイマーと転送状態を進める。
    pub(super) fn tick(&mut self, cpu_cycles: u32, cpu_clock: u32) -> bool {
        let control = self.registers[0x14];
        let irq_was_asserted = self.irq_asserted();
        self.timer_fraction += u64::from(cpu_cycles) * u64::from(OPM_CLOCK);
        let clocks = self.timer_fraction / u64::from(cpu_clock);
        self.timer_fraction %= u64::from(cpu_clock);
        if control & 1 != 0 {
            let mut elapsed = clocks;
            while elapsed >= self.timer_a_counter.max(1) {
                elapsed -= self.timer_a_counter.max(1);
                self.timer_a_counter = self.timer_a_period();
                if control & 4 != 0 {
                    self.status |= 1;
                }
            }
            self.timer_a_counter = self.timer_a_counter.saturating_sub(elapsed);
        }
        if control & 2 != 0 {
            let mut elapsed = clocks;
            while elapsed >= self.timer_b_counter.max(1) {
                elapsed -= self.timer_b_counter.max(1);
                self.timer_b_counter = self.timer_b_period();
                if control & 8 != 0 {
                    self.status |= 2;
                }
            }
            self.timer_b_counter = self.timer_b_counter.saturating_sub(elapsed);
        }
        let irq_is_asserted = self.irq_asserted();
        // YM2151のIRQはMFP GPIP3へ接続されたlevel信号。MFP側へはassert時の
        // edgeだけを渡し、status clearまで同じ割り込みを毎命令再要求しない。
        irq_is_asserted && !irq_was_asserted
    }

    /// YM2151タイマーA設定値から満了までの内部クロック数を算出する。
    fn timer_a_period(&self) -> u64 {
        let value = (u16::from(self.registers[0x10]) << 2) | u16::from(self.registers[0x11] & 3);
        u64::from(1024 - value) * 64
    }

    /// YM2151タイマーB設定値から満了までの内部クロック数を算出する。
    fn timer_b_period(&self) -> u64 {
        u64::from(256 - u16::from(self.registers[0x12])) * 1024
    }

    /// 指定サンプル数のFM・ADPCMステレオPCMを生成する。
    pub(super) fn generate(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        let parameters = self.render_parameters();
        output.reserve(frames * 2);
        for _ in 0..frames {
            let sample = self.resampled_sample(sample_rate, &parameters);
            output.push(f32::from(sample[0]) / 32768.0);
            output.push(f32::from(sample[1]) / 32768.0);
        }
    }

    /// YM2151ネイティブ出力をホストのサンプルレートへ補間する。
    fn resampled_sample(&mut self, sample_rate: u32, parameters: &RenderParameters) -> [i16; 2] {
        if sample_rate == OPM_SAMPLE_RATE {
            return self.native_sample(parameters);
        }

        if !self.resample_started {
            self.resample_current = self.native_sample(parameters);
            self.resample_previous = self.resample_current;
            self.resample_started = true;
        }
        self.resample_accumulator = self.resample_accumulator.saturating_add(OPM_SAMPLE_RATE);
        while self.resample_accumulator >= sample_rate {
            self.resample_accumulator -= sample_rate;
            self.resample_previous = self.resample_current;
            self.resample_current = self.native_sample(parameters);
        }
        let fraction = self.resample_accumulator;
        std::array::from_fn(|channel| {
            let previous = i64::from(self.resample_previous[channel]);
            let current = i64::from(self.resample_current[channel]);
            let interpolated =
                previous * i64::from(sample_rate - fraction) + current * i64::from(fraction);
            (interpolated / i64::from(sample_rate)) as i16
        })
    }

    /// YM2151の全オペレータをネイティブレートで合成する。
    fn native_sample(&mut self, parameters: &RenderParameters) -> [i16; 2] {
        // OPMのEGはFM clock 3回につき1回。x.2 counterの欠番を飛ばす。
        self.envelope_counter = self.envelope_counter.wrapping_add(1);
        if self.envelope_counter & 3 == 3 {
            self.envelope_counter = self.envelope_counter.wrapping_add(1);
        }
        let lfo_pm = self.clock_noise_and_lfo();

        // 全operatorがrelease完了なら、key-on時にphaseがresetされるためPG/EGと
        // algorithm演算を省ける。LFO/noiseだけは無音中も実時間どおり進める。
        if self.channels.iter().all(|channel| {
            channel
                .operators
                .iter()
                .all(|operator| !operator.key_on && operator.envelope == 0x3ff)
        }) {
            for channel in &mut self.channels {
                channel.feedback = [0; 2];
                channel.feedback_input = 0;
            }
            return [0; 2];
        }

        for (channel, channel_parameters) in self.channels.iter_mut().zip(&parameters.channels) {
            channel.feedback[0] = channel.feedback[1];
            channel.feedback[1] = channel.feedback_input;
            for (operator, operator_parameters) in channel
                .operators
                .iter_mut()
                .zip(&channel_parameters.operators)
            {
                if self.envelope_counter & 3 == 0 {
                    clock_envelope(operator, operator_parameters, self.envelope_counter >> 2);
                }
                let phase_step =
                    if channel_parameters.pm_sensitivity == 0 || self.registers[0x1a] & 0x7f == 0 {
                        operator_parameters.phase_step
                    } else {
                        compute_phase_step(
                            operator_parameters,
                            channel_parameters.pm_sensitivity,
                            lfo_pm,
                        )
                    };
                operator.phase = operator.phase.wrapping_add(phase_step);
            }
        }

        let mut output = [0i32; 2];
        for channel_index in 0..8 {
            self.output_channel(
                channel_index,
                &parameters.channels[channel_index],
                &mut output,
            );
        }
        let output = [roundtrip_fp(output[0]), roundtrip_fp(output[1])];
        self.diagnostic_peak = self.diagnostic_peak.max(output[0].unsigned_abs());
        self.diagnostic_peak = self.diagnostic_peak.max(output[1].unsigned_abs());
        output
    }

    /// YM2151のアルゴリズムに従いオペレータを接続して1チャンネルを合成する。
    fn output_channel(
        &mut self,
        channel_index: usize,
        parameters: &ChannelParameters,
        output: &mut [i32; 2],
    ) {
        let am_offset = if parameters.am_sensitivity == 0 {
            0
        } else {
            u16::from(self.lfo_amplitude) << (parameters.am_sensitivity - 1)
        };
        let feedback_level = parameters.control >> 3 & 7;
        let feedback = if feedback_level == 0 {
            0
        } else {
            (i32::from(self.channels[channel_index].feedback[0])
                + i32::from(self.channels[channel_index].feedback[1]))
                >> (10 - feedback_level)
        };
        let channel = &mut self.channels[channel_index];
        let first = operator_output(
            &channel.operators[0],
            &parameters.operators[0],
            feedback,
            am_offset,
        );
        channel.feedback_input = first as i16;

        let algorithm = parameters.control & 7;
        const OP2_INPUT: [usize; 8] = [1, 0, 0, 1, 1, 1, 1, 0];
        const OP3_INPUT: [usize; 8] = [2, 5, 2, 0, 0, 1, 0, 0];
        const OP4_INPUT: [usize; 8] = [3, 3, 6, 7, 3, 1, 0, 0];
        const CARRIERS: [u8; 8] = [0, 0, 0, 0, 0b010, 0b110, 0b110, 0b111];
        let algorithm_index = usize::from(algorithm);
        let mut operator_outputs = [0i16; 8];
        operator_outputs[1] = first as i16;
        operator_outputs[2] = operator_output(
            &channel.operators[1],
            &parameters.operators[1],
            i32::from(operator_outputs[OP2_INPUT[algorithm_index]]) >> 1,
            am_offset,
        ) as i16;
        operator_outputs[5] = operator_outputs[1].wrapping_add(operator_outputs[2]);
        operator_outputs[3] = operator_output(
            &channel.operators[2],
            &parameters.operators[2],
            i32::from(operator_outputs[OP3_INPUT[algorithm_index]]) >> 1,
            am_offset,
        ) as i16;
        operator_outputs[6] = operator_outputs[1].wrapping_add(operator_outputs[3]);
        operator_outputs[7] = operator_outputs[2].wrapping_add(operator_outputs[3]);

        let mut result = if channel_index == 7 && self.registers[0x0f] & 0x80 != 0 {
            let attenuation =
                effective_attenuation(&channel.operators[3], &parameters.operators[3], am_offset);
            let noise = i32::from((attenuation ^ 0x3ff) << 1);
            if self.noise_state != 0 { -noise } else { noise }
        } else {
            operator_output(
                &channel.operators[3],
                &parameters.operators[3],
                i32::from(operator_outputs[OP4_INPUT[algorithm_index]]) >> 1,
                am_offset,
            )
        };
        for (slot, carrier_bit) in [(1usize, 1u8), (2, 2), (3, 4)] {
            if CARRIERS[algorithm_index] & carrier_bit != 0 {
                result = (result + i32::from(operator_outputs[slot])).clamp(-32768, 32767);
            }
        }

        // X68000側で使われるRL順: bit7=left、bit6=right。
        if parameters.control & 0x80 != 0 {
            output[0] += result;
        }
        if parameters.control & 0x40 != 0 {
            output[1] += result;
        }
    }

    /// 現在の映像状態を出力先へ描画し、表示に必要な変換を適用する。
    fn render_parameters(&self) -> RenderParameters {
        // register bank順は M1, M2, C1, C2。接続順 M1, C1, M2, C2へ直す。
        const REGISTER_SLOT: [usize; 4] = [0, 2, 1, 3];
        let channels = std::array::from_fn(|channel_index| {
            let key_code = self.registers[0x28 + channel_index] & 0x7f;
            let key_fraction = self.registers[0x30 + channel_index] >> 2;
            let block_frequency = (u32::from(key_code) << 6) | u32::from(key_fraction);
            let keycode = (block_frequency >> 8) & 0x1f;
            let operators = std::array::from_fn(|slot| {
                let offset = REGISTER_SLOT[slot] * 8 + channel_index;
                let detune_register = self.registers[0x40 + offset] >> 4 & 7;
                let multiple = u32::from(self.registers[0x40 + offset] & 0x0f) * 2;
                let multiple = multiple.max(1);
                let detune = detune_adjustment(detune_register, keycode as usize);
                let key_scale = self.registers[0x80 + offset] >> 6;
                let key_rate = keycode >> (key_scale ^ 3);
                let effective_rate = |raw: u32| {
                    if raw == 0 {
                        0
                    } else {
                        (raw + key_rate).min(63) as u8
                    }
                };
                let sustain = u16::from(self.registers[0xe0 + offset] >> 4);
                let sustain = (sustain | ((sustain + 1) & 0x10)) << 5;
                let mut parameters = OperatorParameters {
                    block_frequency,
                    detune,
                    detune2: self.registers[0xc0 + offset] >> 6 & 3,
                    multiple,
                    phase_step: 0,
                    total_level: u16::from(self.registers[0x60 + offset] & 0x7f) << 3,
                    sustain_level: sustain,
                    envelope_rates: [
                        effective_rate(u32::from(self.registers[0x80 + offset] & 0x1f) * 2),
                        effective_rate(u32::from(self.registers[0xa0 + offset] & 0x1f) * 2),
                        effective_rate(u32::from(self.registers[0xc0 + offset] & 0x1f) * 2),
                        effective_rate(u32::from(self.registers[0xe0 + offset] & 0x0f) * 4 + 2),
                    ],
                    amplitude_modulation: self.registers[0xa0 + offset] & 0x80 != 0,
                };
                parameters.phase_step = compute_phase_step(&parameters, 0, 0);
                parameters
            });
            let sensitivity = self.registers[0x38 + channel_index];
            ChannelParameters {
                control: self.registers[0x20 + channel_index],
                pm_sensitivity: sensitivity >> 4 & 7,
                am_sensitivity: sensitivity & 3,
                operators,
            }
        });
        RenderParameters { channels }
    }

    /// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
    fn clock_noise_and_lfo(&mut self) -> i32 {
        let noise_frequency = (self.registers[0x0f] & 0x1f) ^ 0x1f;
        for _ in 0..2 {
            self.noise_lfsr <<= 1;
            self.noise_lfsr |= ((self.noise_lfsr >> 17) ^ (self.noise_lfsr >> 14) ^ 1) & 1;
            let old_counter = self.noise_counter;
            self.noise_counter = self.noise_counter.wrapping_add(1);
            if old_counter >= noise_frequency {
                self.noise_counter = 0;
                self.noise_state = ((self.noise_lfsr >> 17) & 1) as u8;
            }
        }

        let rate = self.registers[0x18];
        self.lfo_counter = self
            .lfo_counter
            .wrapping_add(u32::from(0x10 | (rate & 0x0f)) << u32::from(rate >> 4));
        if self.registers[0x01] & 2 != 0 {
            self.lfo_counter = 0;
        }
        let index = (self.lfo_counter >> 22) as u8;
        let noise = (self.noise_lfsr >> 17) as u8;
        let (amplitude, phase) = match self.registers[0x1b] & 3 {
            0 => (index ^ 0xff, index as i8),
            1 => {
                let amplitude = if index & 0x80 == 0 { 0xff } else { 0 };
                (amplitude, (amplitude ^ 0x80) as i8)
            }
            2 => {
                let amplitude = if index & 0x80 != 0 {
                    index.wrapping_shl(1)
                } else {
                    (index ^ 0xff).wrapping_shl(1)
                };
                let phase = if index & 0x40 != 0 {
                    amplitude
                } else {
                    !amplitude
                };
                (amplitude, phase as i8)
            }
            _ => (noise, noise as i8),
        };
        self.lfo_amplitude =
            ((u16::from(amplitude) * u16::from(self.registers[0x19] & 0x7f)) >> 7) as u8;
        (i32::from(phase) * i32::from(self.registers[0x1a] & 0x7f)) >> 7
    }

    #[cfg(test)]
    /// 位相カウンタへ変調を加え、サインテーブル参照位置を返す。
    pub(super) fn operator_phase(&self, channel: usize, slot: usize) -> u32 {
        self.channels[channel].operators[slot].phase
    }

    #[cfg(test)]
    /// エンベロープとTLからオペレータの合成減衰量を返す。
    pub(super) fn operator_total_level(&self, channel: usize, slot: usize) -> u16 {
        self.render_parameters().channels[channel].operators[slot].total_level
    }

    /// 現在の状態や結果を利用者向けの診断情報として提示する。
    pub(super) fn diagnostics(&self) -> (u64, u64, u8, u16) {
        let mut active_channels = 0u8;
        for (index, channel) in self.channels.iter().enumerate() {
            if channel
                .operators
                .iter()
                .any(|operator| operator.key_on || operator.envelope < EG_QUIET)
            {
                active_channels |= 1 << index;
            }
        }
        (
            self.diagnostic_register_writes,
            self.diagnostic_key_ons,
            active_channels,
            self.diagnostic_peak,
        )
    }
}

/// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
fn clock_envelope(operator: &mut Operator, parameters: &OperatorParameters, envelope_counter: u32) {
    if operator.envelope_state == EG_ATTACK && operator.envelope == 0 {
        operator.envelope_state = EG_DECAY;
    }
    if operator.envelope_state == EG_DECAY && operator.envelope >= parameters.sustain_level {
        operator.envelope_state = EG_SUSTAIN;
    }
    let rate = u32::from(parameters.envelope_rates[usize::from(operator.envelope_state)]);
    let rate_shift = rate >> 2;
    let shifted_counter = envelope_counter.wrapping_shl(rate_shift);
    if shifted_counter & 0x7ff != 0 {
        return;
    }
    let bit_position = 11.max(rate_shift);
    let index = (shifted_counter >> bit_position) & 7;
    let increment = attenuation_increment(rate as usize, index);
    if operator.envelope_state == EG_ATTACK {
        if rate < 62 {
            let change = ((!(i32::from(operator.envelope))) * i32::from(increment)) >> 4;
            operator.envelope = (i32::from(operator.envelope) + change).clamp(0, 0x3ff) as u16;
        }
    } else {
        operator.envelope = operator.envelope.saturating_add(increment).min(0x3ff);
    }
}

/// エンベロープ・TL・キースケールを合成した減衰量を返す。
fn effective_attenuation(
    operator: &Operator,
    parameters: &OperatorParameters,
    am_offset: u16,
) -> u16 {
    let am = if parameters.amplitude_modulation {
        am_offset
    } else {
        0
    };
    operator
        .envelope
        .saturating_add(parameters.total_level)
        .saturating_add(am)
        .min(0x3ff)
}

/// オペレータの位相と減衰量から現在のFM出力値を計算する。
fn operator_output(
    operator: &Operator,
    parameters: &OperatorParameters,
    modulation: i32,
    am_offset: u16,
) -> i32 {
    let attenuation = effective_attenuation(operator, parameters, am_offset);
    if attenuation > EG_QUIET {
        return 0;
    }
    let phase = ((operator.phase >> 10) as i32).wrapping_add(modulation) as u32 & 0x3ff;
    let mirrored = if phase & 0x100 != 0 { !phase } else { phase };
    let sine_attenuation = u32::from(SINE_ATTENUATION[(mirrored & 0xff) as usize]);
    let logarithmic = sine_attenuation + (u32::from(attenuation) << 2);
    let shift = logarithmic >> 8;
    let volume = if shift >= 16 {
        0
    } else {
        i32::from(ATTENUATION_VOLUME[(logarithmic & 0xff) as usize] >> shift)
    };
    if phase & 0x200 != 0 { -volume } else { volume }
}

/// YM2151のエンベロープレートと位相から今回の減衰増分を返す。
fn attenuation_increment(rate: usize, index: u32) -> u16 {
    const INCREMENTS: [u32; 64] = [
        0x00000000, 0x00000000, 0x10101010, 0x10101010, 0x10101010, 0x10101010, 0x11101110,
        0x11101110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010,
        0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
        0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110,
        0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110,
        0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010,
        0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x11111111,
        0x21112111, 0x21212121, 0x22212221, 0x22222222, 0x42224222, 0x42424242, 0x44424442,
        0x44444444, 0x84448444, 0x84848484, 0x88848884, 0x88888888, 0x88888888, 0x88888888,
        0x88888888,
    ];
    ((INCREMENTS[rate] >> (4 * index)) & 0x0f) as u16
}

/// DT1設定とキーコードから位相増分の補正値を計算する。
fn detune_adjustment(detune: u8, keycode: usize) -> i32 {
    const DETUNE: [[u8; 4]; 32] = [
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 0, 1, 2],
        [0, 1, 2, 2],
        [0, 1, 2, 3],
        [0, 1, 2, 3],
        [0, 1, 2, 3],
        [0, 1, 2, 4],
        [0, 1, 3, 4],
        [0, 1, 3, 4],
        [0, 1, 3, 5],
        [0, 2, 4, 5],
        [0, 2, 4, 6],
        [0, 2, 4, 6],
        [0, 2, 5, 7],
        [0, 2, 5, 8],
        [0, 3, 6, 8],
        [0, 3, 6, 9],
        [0, 3, 7, 10],
        [0, 4, 8, 11],
        [0, 4, 8, 12],
        [0, 4, 9, 13],
        [0, 5, 10, 14],
        [0, 5, 11, 16],
        [0, 6, 12, 17],
        [0, 6, 13, 19],
        [0, 7, 14, 20],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
        [0, 8, 16, 22],
    ];
    let adjustment = i32::from(DETUNE[keycode][usize::from(detune & 3)]);
    if detune & 4 != 0 {
        -adjustment
    } else {
        adjustment
    }
}

/// キーコード・倍率・デチューンからオペレータの位相増分を計算する。
fn compute_phase_step(parameters: &OperatorParameters, pm_sensitivity: u8, lfo_pm: i32) -> u32 {
    const DETUNE2: [i32; 4] = [0, 384, 500, 608];
    let mut delta = DETUNE2[usize::from(parameters.detune2)];
    if pm_sensitivity != 0 {
        delta += if pm_sensitivity < 6 {
            lfo_pm >> (6 - pm_sensitivity)
        } else {
            lfo_pm << (pm_sensitivity - 5)
        };
    }
    let base = phase_step_from_key_code(parameters.block_frequency, delta);
    let detuned = (i64::from(base) + i64::from(parameters.detune)).max(0) as u32;
    detuned.saturating_mul(parameters.multiple) >> 1
}

/// キーコードと周波数分数から基準位相増分を計算する。
fn phase_step_from_key_code(block_frequency: u32, delta: i32) -> u32 {
    let mut block = ((block_frequency >> 10) & 7) as i32;
    let code = ((block_frequency >> 6) & 0x0f) as i32;
    let adjusted_code = code - ((block_frequency >> 8) & 3) as i32;
    let mut effective = (adjusted_code << 6) | (block_frequency & 0x3f) as i32;
    effective += delta;
    while effective < 0 {
        if block == 0 {
            return PHASE_STEP[0] >> 7;
        }
        effective += 768;
        block -= 1;
    }
    while effective >= 768 {
        if block >= 7 {
            return PHASE_STEP[767];
        }
        effective -= 768;
        block += 1;
    }
    PHASE_STEP[effective as usize] >> (block ^ 7)
}

/// 入力値を `roundtrip_fp` に対応する内部表現へ変換して返す。
fn roundtrip_fp(value: i32) -> i16 {
    let value = value.clamp(-32768, 32767);
    let scan = value ^ (value >> 31);
    let exponent = (7i32 - (scan << 17).leading_zeros() as i32).max(1) - 1;
    let mask = (1 << exponent) - 1;
    (value & !mask) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_register(chip: &mut Ym2151, register: u8, value: u8) {
        chip.write(1, register);
        chip.write(3, value);
    }

    #[test]
    /// `phase_table_uses_x68000_four_megahertz_clock` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn phase_table_uses_x68000_four_megahertz_clock() {
        let block_frequency = u32::from(0x48u8) << 6;
        let step = phase_step_from_key_code(block_frequency, 0);
        let frequency = f64::from(step) * f64::from(OPM_SAMPLE_RATE) / f64::from(1 << 20);
        assert!((frequency - 438.0).abs() < 1.0);
    }

    #[test]
    /// `operator_register_order_is_m1_c1_m2_c2` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn operator_register_order_is_m1_c1_m2_c2() {
        let mut chip = Ym2151::default();
        chip.registers[0x68] = 20;
        chip.registers[0x70] = 40;
        assert_eq!(chip.operator_total_level(0, 1), 40 << 3);
        assert_eq!(chip.operator_total_level(0, 2), 20 << 3);
    }

    #[test]
    /// `timer_status_requires_irq_enable_and_load_edge_restarts_period` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn timer_status_requires_irq_enable_and_load_edge_restarts_period() {
        let mut chip = Ym2151::default();
        write_register(&mut chip, 0x10, 0xff);
        write_register(&mut chip, 0x11, 3);

        // loadだけでは満了してもstatusを立てない。
        write_register(&mut chip, 0x14, 0x01);
        assert!(!chip.tick(64, OPM_CLOCK));
        assert_eq!(chip.read(1) & 1, 0);

        // loadを維持したままIRQ enableすると次の満了でstatus/IRQが立つ。
        write_register(&mut chip, 0x14, 0x05);
        assert!(!chip.tick(63, OPM_CLOCK));
        assert!(chip.tick(1, OPM_CLOCK));
        assert_eq!(chip.read(1) & 1, 1);
        assert_eq!(chip.read(3) & 1, 1);

        // stopしてから再loadすると途中のcounterを再利用しない。
        write_register(&mut chip, 0x14, 0x10);
        write_register(&mut chip, 0x10, 0xff);
        write_register(&mut chip, 0x11, 2); // 128 clocks
        write_register(&mut chip, 0x14, 0x05);
        assert!(!chip.tick(64, OPM_CLOCK));
        write_register(&mut chip, 0x14, 0x00);
        write_register(&mut chip, 0x14, 0x05);
        assert!(!chip.tick(64, OPM_CLOCK));
        assert!(chip.tick(64, OPM_CLOCK));
    }

    #[test]
    /// `running_timer_applies_new_period_after_current_expiry` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn running_timer_applies_new_period_after_current_expiry() {
        let mut chip = Ym2151::default();
        write_register(&mut chip, 0x10, 0xff);
        write_register(&mut chip, 0x11, 2); // current period: 128 clocks
        write_register(&mut chip, 0x14, 0x05);
        assert!(!chip.tick(64, OPM_CLOCK));
        write_register(&mut chip, 0x11, 3); // next period: 64 clocks
        assert!(!chip.tick(63, OPM_CLOCK));
        assert!(chip.tick(1, OPM_CLOCK));
        assert!(
            !chip.tick(1, OPM_CLOCK),
            "asserted IRQ must not retrigger without a clear"
        );
        write_register(&mut chip, 0x14, 0x15);
        assert!(!chip.tick(62, OPM_CLOCK));
        assert!(chip.tick(1, OPM_CLOCK));
    }
}
