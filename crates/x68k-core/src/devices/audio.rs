//! YM2151 (OPM) と MSM6258 ADPCM の決定論的ステレオ音源。
//!
//! レジスタ配置、タイマ接続、ADPCM差分表は PX68k の `fmgen` ラッパと
//! `x68k/adpcm.c` を比較資料としている。FM演算部はRustで独立実装している。

use std::collections::VecDeque;
use std::f64::consts::TAU;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Operator {
    phase: f64,
    envelope: f64,
    stage: u8,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Channel {
    operators: [Operator; 4],
    feedback: [f64; 2],
}

#[derive(Debug, Clone, Copy)]
struct EnvelopeParameters {
    attack_coefficient: f64,
    decay_factor: f64,
    sustain_factor: f64,
    sustain_level: f64,
    release_factor: f64,
}

#[derive(Debug, Clone, Copy)]
struct OperatorParameters {
    phase_step: f64,
    attenuation: f64,
    amplitude_modulation: bool,
    envelope: EnvelopeParameters,
}

#[derive(Debug, Clone, Copy)]
struct ChannelParameters {
    control: u8,
    algorithm: u8,
    pitch_sensitivity: f64,
    amplitude_sensitivity: f64,
    operators: [OperatorParameters; 4],
}

#[derive(Debug, Clone)]
struct RenderParameters {
    channels: [ChannelParameters; 8],
    lfo_step: f64,
    noise_step: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Ym2151 {
    registers: Vec<u8>,
    address: u8,
    channels: [Channel; 8],
    timer_a_counter: u64,
    timer_b_counter: u64,
    timer_fraction: u64,
    status: u8,
    lfo_phase: f64,
    noise_phase: f64,
    noise_lfsr: u32,
    amplitude_depth: u8,
    phase_depth: u8,
}

impl Default for Ym2151 {
    fn default() -> Self {
        Self {
            registers: vec![0; 256],
            address: 0,
            channels: [Channel::default(); 8],
            timer_a_counter: 0,
            timer_b_counter: 0,
            timer_fraction: 0,
            status: 0,
            lfo_phase: 0.0,
            noise_phase: 0.0,
            noise_lfsr: 0x1ffff,
            amplitude_depth: 0,
            phase_depth: 0,
        }
    }
}

impl Ym2151 {
    fn silent(&self) -> bool {
        self.channels
            .iter()
            .all(|channel| channel.operators.iter().all(|operator| operator.stage == 0))
    }

    fn advance_silent(&mut self, frames: usize, parameters: &RenderParameters) {
        // LFO/noiseはkey-off中も実時間で進む。FM operator演算だけを省き、
        // sample単位の加算順序は通常経路と揃えてsave/load後も決定論的にする。
        for _ in 0..frames {
            self.advance_lfo(parameters.lfo_step);
            self.advance_noise(parameters.noise_step);
        }
        for channel in &mut self.channels {
            channel.feedback = [0.0; 2];
        }
    }

    fn write(&mut self, offset: u32, value: u8) {
        match offset & 3 {
            1 => self.address = value,
            3 => {
                let register = self.address as usize;
                self.registers[register] = value;
                if register == 0x08 {
                    let channel = usize::from(value & 7);
                    for slot in 0..4 {
                        let keyed = value & (1 << (slot + 3)) != 0;
                        let operator = &mut self.channels[channel].operators[slot];
                        if keyed {
                            if operator.stage == 0 || operator.stage == 4 {
                                operator.envelope = 0.0;
                                // OPMはkey-onの立上りでphase generatorもresetする。
                                operator.phase = 0.0;
                                operator.stage = 1;
                            }
                        } else if operator.stage != 0 {
                            operator.stage = 4;
                        }
                    }
                } else if register == 0x14 {
                    if value & 0x10 != 0 {
                        self.status &= !1;
                    }
                    if value & 0x20 != 0 {
                        self.status &= !2;
                    }
                } else if register == 0x19 {
                    if value & 0x80 != 0 {
                        self.phase_depth = value & 0x7f;
                    } else {
                        self.amplitude_depth = value & 0x7f;
                    }
                }
            }
            _ => {}
        }
    }

    fn read(&self, offset: u32) -> u8 {
        if offset & 3 == 3 { self.status } else { 0 }
    }

    fn tick(&mut self, cpu_cycles: u32, cpu_clock: u32) -> bool {
        self.timer_fraction += u64::from(cpu_cycles) * 4_000_000;
        let clocks = self.timer_fraction / u64::from(cpu_clock);
        self.timer_fraction %= u64::from(cpu_clock);
        let control = self.registers[0x14];
        if control & 1 != 0 {
            self.timer_a_counter += clocks;
            let value =
                (u16::from(self.registers[0x10]) << 2) | u16::from(self.registers[0x11] & 3);
            let period = u64::from(1024 - value) * 64;
            while self.timer_a_counter >= period.max(1) {
                self.timer_a_counter -= period.max(1);
                self.status |= 1;
            }
        }
        if control & 2 != 0 {
            self.timer_b_counter += clocks;
            let period = u64::from(256 - u16::from(self.registers[0x12])) * 1024;
            while self.timer_b_counter >= period.max(1) {
                self.timer_b_counter -= period.max(1);
                self.status |= 2;
            }
        }
        self.status & ((control >> 2) & 3) != 0
    }

    fn render_parameters(&self, sample_rate: u32) -> RenderParameters {
        const PITCH_SENSITIVITY: [f64; 8] = [0.0, 0.06, 0.12, 0.25, 0.5, 1.0, 2.0, 4.0];
        // OPMのAMS 1/2/3は約23.9/47.8/95.6dB。
        const AMPLITUDE_SENSITIVITY: [f64; 4] = [0.0, 1.0, 2.0, 4.0];
        // OPMのoperator register順はM1,M2,C1,C2だが、algorithm計算は
        // M1,C1,M2,C2の自然な接続順で保持する。
        const REGISTER_SLOT: [usize; 4] = [0, 2, 1, 3];
        let sample_rate = f64::from(sample_rate);
        let channels = std::array::from_fn(|channel_index| {
            let key_code = self.registers[0x28 + channel_index];
            let base_frequency = channel_frequency(key_code, self.registers[0x30 + channel_index]);
            let control = self.registers[0x20 + channel_index];
            let sensitivity = self.registers[0x38 + channel_index];
            let operators = std::array::from_fn(|slot| {
                let register_offset = REGISTER_SLOT[slot] * 8 + channel_index;
                let multiply = self.registers[0x40 + register_offset] & 0x0f;
                let multiply = if multiply == 0 {
                    0.5
                } else {
                    f64::from(multiply)
                };
                let total_level = self.registers[0x60 + register_offset] & 0x7f;
                OperatorParameters {
                    phase_step: TAU
                        * base_frequency
                        * multiply
                        * operator_detune(&self.registers, register_offset)
                        / sample_rate,
                    attenuation: 10f64.powf(-(f64::from(total_level) * 0.75) / 20.0),
                    amplitude_modulation: self.registers[0xa0 + register_offset] & 0x80 != 0,
                    envelope: envelope_parameters(
                        &self.registers,
                        register_offset,
                        key_code,
                        sample_rate,
                    ),
                }
            });
            ChannelParameters {
                control,
                algorithm: control & 7,
                pitch_sensitivity: PITCH_SENSITIVITY[usize::from(sensitivity >> 4 & 7)],
                amplitude_sensitivity: AMPLITUDE_SENSITIVITY[usize::from(sensitivity & 3)],
                operators,
            }
        });
        let lfo_register = self.registers[0x18];
        let lfo_frequency = 0.004
            * f64::from(1u32 << u32::from(lfo_register >> 4))
            * (1.0 + f64::from(lfo_register & 0x0f) / 16.0);
        let noise_period = 32 - u32::from(self.registers[0x0f] & 0x1f);
        RenderParameters {
            channels,
            lfo_step: lfo_frequency / sample_rate,
            noise_step: 4_000_000.0 / (64.0 * f64::from(noise_period.max(1))) / sample_rate,
        }
    }

    fn sample(&mut self, parameters: &RenderParameters) -> (f32, f32) {
        let (lfo_pm, lfo_am) = self.advance_lfo(parameters.lfo_step);
        let noise = self.advance_noise(parameters.noise_step);
        let mut left = 0.0;
        let mut right = 0.0;
        for channel_index in 0..8 {
            if self.channels[channel_index]
                .operators
                .iter()
                .all(|operator| operator.stage == 0)
            {
                self.channels[channel_index].feedback = [0.0; 2];
                continue;
            }
            let channel_parameters = &parameters.channels[channel_index];
            let phase_semitones =
                lfo_pm * f64::from(self.phase_depth) / 127.0 * channel_parameters.pitch_sensitivity;
            let pitch_ratio = pitch_ratio(phase_semitones);
            let amplitude_lfo = lfo_am * f64::from(self.amplitude_depth) / 127.0
                * channel_parameters.amplitude_sensitivity;
            let mut levels = [0.0; 4];
            for slot in 0..4 {
                let operator_parameters = &channel_parameters.operators[slot];
                let operator = &mut self.channels[channel_index].operators[slot];
                advance_envelope(operator, &operator_parameters.envelope);
                operator.phase += operator_parameters.phase_step * pitch_ratio;
                if operator.phase >= TAU {
                    operator.phase -= TAU;
                    if operator.phase >= TAU {
                        operator.phase %= TAU;
                    }
                }
                let am_gain = if operator_parameters.amplitude_modulation {
                    amplitude_gain(amplitude_lfo)
                } else {
                    1.0
                };
                levels[slot] = operator.envelope * operator_parameters.attenuation * am_gain;
            }
            let feedback_level = (channel_parameters.control >> 3) & 7;
            let feedback = if feedback_level == 0 {
                0.0
            } else {
                const FEEDBACK_SCALE: [f64; 8] = [0.0, 0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 4.0];
                let history = self.channels[channel_index].feedback;
                (history[0] + history[1]) * FEEDBACK_SCALE[usize::from(feedback_level)]
            };
            let phases =
                std::array::from_fn(|slot| self.channels[channel_index].operators[slot].phase);
            let (output, first) =
                algorithm_output(channel_parameters.algorithm, phases, levels, feedback);
            self.channels[channel_index].feedback[1] = self.channels[channel_index].feedback[0];
            self.channels[channel_index].feedback[0] = first;
            let output = if channel_index == 7 && self.registers[0x0f] & 0x80 != 0 {
                // OPMのnoiseはchannel 7最終operatorを置換する。
                output + noise * levels[3]
            } else {
                output
            };
            // OPM RL bitsはbit7=left、bit6=right。
            if channel_parameters.control & 0x80 != 0 {
                left += output;
            }
            if channel_parameters.control & 0x40 != 0 {
                right += output;
            }
        }
        ((left * 0.08) as f32, (right * 0.08) as f32)
    }

    fn advance_lfo(&mut self, step: f64) -> (f64, f64) {
        self.lfo_phase += step;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase %= 1.0;
        }
        match self.registers[0x1b] & 3 {
            // 戻り値は(PMのbipolar値, AMのunipolar値)。
            0 => (1.0 - self.lfo_phase * 2.0, 1.0 - self.lfo_phase),
            1 => {
                if self.lfo_phase < 0.5 {
                    (1.0, 1.0)
                } else {
                    (-1.0, 0.0)
                }
            }
            2 => (
                1.0 - (self.lfo_phase * 4.0 - 2.0).abs(),
                (self.lfo_phase * 2.0 - 1.0).abs(),
            ),
            _ => {
                let value = f64::from((self.noise_lfsr & 0xffff) as u16) / 65535.0;
                (value * 2.0 - 1.0, value)
            }
        }
    }

    fn advance_noise(&mut self, step: f64) -> f64 {
        self.noise_phase += step;
        while self.noise_phase >= 1.0 {
            self.noise_phase -= 1.0;
            let feedback = (self.noise_lfsr ^ (self.noise_lfsr >> 3)) & 1;
            self.noise_lfsr = (self.noise_lfsr >> 1) | (feedback << 16);
        }
        if self.noise_lfsr & 1 == 0 { -1.0 } else { 1.0 }
    }
}

fn channel_frequency(key_code: u8, key_fraction: u8) -> f64 {
    const NOTES: [i16; 16] = [0, 1, 2, 2, 3, 4, 5, 5, 6, 7, 8, 8, 9, 10, 11, 11];
    let octave = i16::from(key_code >> 4);
    let note = NOTES[usize::from(key_code & 0x0f)];
    let midi = 12 * (octave + 1) + note;
    let fraction = f64::from(key_fraction >> 2) / 64.0;
    440.0 * 2f64.powf((f64::from(midi) + fraction - 69.0) / 12.0)
}

fn envelope_parameters(
    registers: &[u8],
    offset: usize,
    key_code: u8,
    sample_rate: f64,
) -> EnvelopeParameters {
    let key_scale = registers[0x80 + offset] >> 6;
    let key_rate = if key_scale == 0 {
        0
    } else {
        (key_code >> (4 - key_scale)).min(31)
    };
    let effective_rate = |raw: u8| {
        if raw == 0 {
            0
        } else {
            raw.saturating_mul(2).saturating_add(key_rate).min(63)
        }
    };
    let attack = effective_rate(registers[0x80 + offset] & 0x1f);
    let decay = effective_rate(registers[0xa0 + offset] & 0x1f);
    let sustain_rate = effective_rate(registers[0xc0 + offset] & 0x1f);
    let sustain_level = (registers[0xe0 + offset] >> 4).min(15);
    let release = ((registers[0xe0 + offset] & 0x0f) * 4 + 2)
        .saturating_add(key_rate)
        .min(63);
    // Yamaha EGのrateは4増えるごとに概ね2倍速になる。0は停止で、
    // attack 62/63だけはkey-on時に即座に最大levelへ到達する。
    let duration = |rate: u8| 20.0 * 2f64.powf((4.0 - f64::from(rate)) / 4.0);
    let decay_factor = |rate: u8| {
        if rate == 0 {
            1.0
        } else {
            // durationの間に60dB（amplitude 1/1000）減衰する係数。
            (-std::f64::consts::LN_10 * 3.0 / (duration(rate) * sample_rate)).exp()
        }
    };
    let attack_coefficient = if attack == 0 {
        0.0
    } else if attack >= 62 {
        1.0
    } else {
        1.0 - (-6.0 / (duration(attack) * sample_rate)).exp()
    };
    let sustain_db = if sustain_level == 15 {
        93.0
    } else {
        f64::from(sustain_level) * 3.0
    };
    EnvelopeParameters {
        attack_coefficient,
        decay_factor: decay_factor(decay),
        sustain_factor: decay_factor(sustain_rate),
        sustain_level: 10f64.powf(-sustain_db / 20.0),
        release_factor: decay_factor(release),
    }
}

fn advance_envelope(operator: &mut Operator, parameters: &EnvelopeParameters) {
    match operator.stage {
        1 => {
            operator.envelope += (1.0 - operator.envelope) * parameters.attack_coefficient;
            if operator.envelope >= 1.0 - 1e-9 {
                operator.envelope = 1.0;
                operator.stage = 2;
            }
        }
        2 => {
            operator.envelope *= parameters.decay_factor;
            if operator.envelope <= parameters.sustain_level {
                operator.envelope = parameters.sustain_level;
                operator.stage = 3;
            }
        }
        3 => operator.envelope *= parameters.sustain_factor,
        4 => {
            operator.envelope *= parameters.release_factor;
            if operator.envelope < 1e-6 {
                operator.envelope = 0.0;
                operator.stage = 0;
            }
        }
        _ => operator.envelope = 0.0,
    }
}

const OPM_SINE_STEPS: usize = 1024;
const MODULATION_STEPS_PER_SEMITONE: f64 = 256.0;
const MODULATION_SEMITONES: f64 = 4.0;
const MODULATION_TABLE_SIZE: usize =
    (MODULATION_STEPS_PER_SEMITONE * MODULATION_SEMITONES * 2.0) as usize + 1;
const AMPLITUDE_TABLE_SIZE: usize = 1025;

fn opm_sine(phase: f64) -> f64 {
    static TABLE: OnceLock<[f64; OPM_SINE_STEPS]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|index| (TAU * index as f64 / OPM_SINE_STEPS as f64).sin())
    });
    let step = (phase * OPM_SINE_STEPS as f64 / TAU).floor() as i64;
    table[(step & (OPM_SINE_STEPS as i64 - 1)) as usize]
}

fn pitch_ratio(semitones: f64) -> f64 {
    if semitones == 0.0 {
        return 1.0;
    }
    static TABLE: OnceLock<[f64; MODULATION_TABLE_SIZE]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|index| {
            let semitones = index as f64 / MODULATION_STEPS_PER_SEMITONE - MODULATION_SEMITONES;
            2f64.powf(semitones / 12.0)
        })
    });
    let index = ((semitones.clamp(-MODULATION_SEMITONES, MODULATION_SEMITONES)
        + MODULATION_SEMITONES)
        * MODULATION_STEPS_PER_SEMITONE)
        .round() as usize;
    table[index]
}

fn amplitude_gain(amplitude: f64) -> f64 {
    static TABLE: OnceLock<[f64; AMPLITUDE_TABLE_SIZE]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|index| {
            let attenuation_db = index as f64 / 256.0 * 23.9;
            10f64.powf(-attenuation_db / 20.0)
        })
    });
    let index = (amplitude.clamp(0.0, 4.0) * 256.0).round() as usize;
    table[index]
}

fn operator_detune(registers: &[u8], offset: usize) -> f64 {
    const DT1_CENTS: [f64; 8] = [0.0, 3.4, 6.7, 10.0, 0.0, -3.4, -6.7, -10.0];
    const DT2_CENTS: [f64; 4] = [0.0, 600.0, 781.0, 950.0];
    let dt1 = usize::from(registers[0x40 + offset] >> 4 & 7);
    let dt2 = usize::from(registers[0xc0 + offset] >> 6 & 3);
    2f64.powf((DT1_CENTS[dt1] + DT2_CENTS[dt2]) / 1200.0)
}

fn algorithm_output(algorithm: u8, phase: [f64; 4], level: [f64; 4], feedback: f64) -> (f64, f64) {
    let op =
        |index: usize, modulation: f64| opm_sine(phase[index] + modulation * 4.0) * level[index];
    let first = op(0, feedback);
    let output = match algorithm {
        0 => op(3, op(2, op(1, first))),
        1 => op(3, op(2, first + op(1, 0.0))),
        2 => op(3, first + op(2, op(1, 0.0))),
        3 => op(3, op(1, first) + op(2, 0.0)),
        4 => op(1, first) + op(3, op(2, 0.0)),
        5 => op(1, first) + op(2, first) + op(3, first),
        6 => op(1, first) + op(2, 0.0) + op(3, 0.0),
        _ => first + op(1, 0.0) + op(2, 0.0) + op(3, 0.0),
    };
    (output, first)
}

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
    fn silent(&self) -> bool {
        !self.playing && self.current == 0
    }

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

    fn read(&self, offset: u32) -> u8 {
        if offset == 1 {
            if self.playing { 0xc0 } else { 0x40 }
        } else {
            0
        }
    }

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
    pub(crate) fn read_ym(&self, offset: u32) -> u8 {
        self.ym2151.read(offset)
    }

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

    pub(crate) fn read_adpcm(&self, offset: u32) -> u8 {
        self.msm6258.read(offset)
    }

    pub(crate) fn write_adpcm(&mut self, offset: u32, value: u8) {
        self.render_to_cursor();
        self.msm6258.write(offset, value);
    }

    pub(crate) fn set_pan(&mut self, value: u8) {
        self.render_to_cursor();
        self.msm6258.pan = value & 0x0f;
        self.msm6258.clock_select = (self.msm6258.clock_select & 4) | ((value >> 2) & 3);
    }

    pub(crate) fn tick(&mut self, cycles: u32, cpu_clock: u32) -> bool {
        let irq = self.ym2151.tick(cycles, cpu_clock);
        if self.frame.active {
            self.frame.elapsed_cycles = self.frame.elapsed_cycles.saturating_add(cycles);
            self.frame.instruction_offset = 0;
        }
        irq
    }

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

    pub(crate) fn set_instruction_offset(&mut self, cycles: u32) {
        if self.frame.active {
            self.frame.instruction_offset = cycles;
        }
    }

    pub(crate) fn generate(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        output.reserve(frames * 2);
        // register write間ではOPM parameterは不変。pow/除算をsampleごとに
        // 繰り返さず、frame内writeの直前でblockを分割する。
        let parameters = self.ym2151.render_parameters(sample_rate);
        if self.ym2151.silent() && self.msm6258.silent() {
            self.ym2151.advance_silent(frames, &parameters);
            output.resize(output.len() + frames * 2, 0.0);
            return;
        }
        for _ in 0..frames {
            let fm = self.ym2151.sample(&parameters);
            let adpcm = self.msm6258.sample(sample_rate);
            output.push((fm.0 + adpcm.0 * 0.5).clamp(-1.0, 1.0));
            output.push((fm.1 + adpcm.1 * 0.5).clamp(-1.0, 1.0));
        }
    }

    pub(crate) fn finish_frame(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        if !self.frame.active {
            self.generate(frames, sample_rate, output);
            return;
        }
        self.render_to_frame(frames);
        output.append(&mut self.frame.output);
        self.frame = AudioFrame::default();
    }

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

    fn write_ym(audio: &mut AudioSystem, register: u8, value: u8) {
        audio.write_ym(1, register);
        audio.write_ym(3, value);
    }

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

    fn configure_single_carrier(audio: &mut AudioSystem, control: u8) {
        write_ym(audio, 0x20, control | 7);
        write_ym(audio, 0x28, 0x4c);
        write_ym(audio, 0x40, 1);
        write_ym(audio, 0x60, 0);
        write_ym(audio, 0x80, 0x1f);
    }

    #[test]
    fn ym2151_key_on_generates_stereo_pcm() {
        let mut audio = AudioSystem::default();
        key_on_channel(&mut audio, 0, 7);
        let mut samples = Vec::new();
        audio.generate(512, 48_000, &mut samples);
        assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
        assert_eq!(samples.len(), 1024);
    }

    #[test]
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
    fn msm6258_uses_the_chip_integer_difference_table() {
        let mut chip = Msm6258::default();
        chip.decode(0x07);
        assert_eq!((chip.predictor, chip.step), (30, 8));
        chip.decode(0x0f);
        assert_eq!((chip.predictor, chip.step), (-33, 16));
    }

    #[test]
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
    fn ym2151_operator_register_order_matches_opm_wiring() {
        let mut chip = Ym2151::default();
        chip.registers[0x68] = 20; // register bank M2
        chip.registers[0x70] = 40; // register bank C1
        let parameters = chip.render_parameters(48_000);
        let expected_c1 = 10f64.powf(-(40.0 * 0.75) / 20.0);
        let expected_m2 = 10f64.powf(-(20.0 * 0.75) / 20.0);
        assert!((parameters.channels[0].operators[1].attenuation - expected_c1).abs() < 1e-12);
        assert!((parameters.channels[0].operators[2].attenuation - expected_m2).abs() < 1e-12);
    }

    #[test]
    fn ym2151_key_on_edge_resets_phase_without_retriggering_held_key() {
        let mut audio = AudioSystem::default();
        key_on_channel(&mut audio, 0, 7);
        let mut samples = Vec::new();
        audio.generate(16, 48_000, &mut samples);
        let running_phase = audio.ym2151.channels[0].operators[0].phase;
        assert!(running_phase > 0.0);

        write_ym(&mut audio, 0x08, 0x78);
        assert_eq!(audio.ym2151.channels[0].operators[0].phase, running_phase);
        write_ym(&mut audio, 0x08, 0x00);
        write_ym(&mut audio, 0x08, 0x78);
        assert_eq!(audio.ym2151.channels[0].operators[0].phase, 0.0);
    }

    #[test]
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
