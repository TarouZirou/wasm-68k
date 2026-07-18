//! YM2151 (OPM) と MSM6258 ADPCM の決定論的ステレオ音源。
//!
//! レジスタ配置、タイマ接続、ADPCM差分表は PX68k の `fmgen` ラッパと
//! `x68k/adpcm.c` を比較資料としている。FM演算部はRustで独立実装している。

use std::collections::VecDeque;
use std::f64::consts::TAU;

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
                            }
                            operator.stage = 1;
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

    fn sample(&mut self, sample_rate: u32) -> (f32, f32) {
        let lfo = self.advance_lfo(sample_rate);
        let noise = self.advance_noise(sample_rate);
        let mut left = 0.0;
        let mut right = 0.0;
        for channel_index in 0..8 {
            let base_frequency = channel_frequency(
                self.registers[0x28 + channel_index],
                self.registers[0x30 + channel_index],
            );
            let control = self.registers[0x20 + channel_index];
            let algorithm = control & 7;
            let sensitivity = self.registers[0x38 + channel_index];
            let pms = (sensitivity >> 4) & 7;
            let ams = sensitivity & 3;
            let phase_semitones = lfo * f64::from(self.phase_depth) / 127.0
                * [0.0, 0.06, 0.12, 0.25, 0.5, 1.0, 2.0, 4.0][usize::from(pms)];
            let frequency = base_frequency * 2f64.powf(phase_semitones / 12.0);
            let amplitude_lfo = lfo.abs() * f64::from(self.amplitude_depth) / 127.0
                * [0.0, 0.25, 0.5, 1.0][usize::from(ams)];
            let mut levels = [0.0; 4];
            for slot in 0..4 {
                let register_offset = slot * 8 + channel_index;
                let multiply = self.registers[0x40 + register_offset] & 0x0f;
                let multiply = if multiply == 0 {
                    0.5
                } else {
                    f64::from(multiply)
                };
                let total_level = self.registers[0x60 + register_offset] & 0x7f;
                let attenuation = 10f64.powf(-(f64::from(total_level) * 0.75) / 20.0);
                let operator = &mut self.channels[channel_index].operators[slot];
                advance_envelope(
                    operator,
                    &self.registers,
                    register_offset,
                    self.registers[0x28 + channel_index],
                    sample_rate,
                );
                let detune = operator_detune(&self.registers, register_offset);
                operator.phase = (operator.phase
                    + TAU * frequency * multiply * detune / f64::from(sample_rate))
                    % TAU;
                let am_enabled = self.registers[0xa0 + register_offset] & 0x80 != 0;
                let am_gain = if am_enabled {
                    10f64.powf(-(amplitude_lfo * 24.0) / 20.0)
                } else {
                    1.0
                };
                levels[slot] = operator.envelope * attenuation * am_gain;
            }
            let feedback_level = (control >> 3) & 7;
            let feedback = if feedback_level == 0 {
                0.0
            } else {
                let history = self.channels[channel_index].feedback;
                (history[0] + history[1]) * 0.5 * 2f64.powi(i32::from(feedback_level) - 4)
            };
            let phases =
                std::array::from_fn(|slot| self.channels[channel_index].operators[slot].phase);
            let (output, first) = algorithm_output(algorithm, phases, levels, feedback);
            self.channels[channel_index].feedback[1] = self.channels[channel_index].feedback[0];
            self.channels[channel_index].feedback[0] = first;
            let output = if channel_index == 7 && self.registers[0x0f] & 0x80 != 0 {
                // OPMのnoiseはchannel 7最終operatorを置換する。
                output + noise * levels[3]
            } else {
                output
            };
            if control & 0x40 != 0 {
                left += output;
            }
            if control & 0x80 != 0 {
                right += output;
            }
        }
        ((left * 0.08) as f32, (right * 0.08) as f32)
    }

    fn advance_lfo(&mut self, sample_rate: u32) -> f64 {
        let register = self.registers[0x18];
        let exponent = i32::from(register >> 4);
        let fraction = 1.0 + f64::from(register & 0x0f) / 16.0;
        let frequency = 0.004 * 2f64.powi(exponent) * fraction;
        self.lfo_phase = (self.lfo_phase + frequency / f64::from(sample_rate)) % 1.0;
        match self.registers[0x1b] & 3 {
            0 => 1.0 - self.lfo_phase * 2.0,
            1 => {
                if self.lfo_phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            2 => 1.0 - (self.lfo_phase * 4.0 - 2.0).abs(),
            _ => f64::from((self.noise_lfsr & 0xffff) as u16) / 32767.5 - 1.0,
        }
    }

    fn advance_noise(&mut self, sample_rate: u32) -> f64 {
        let period = 32 - u32::from(self.registers[0x0f] & 0x1f);
        let frequency = 4_000_000.0 / (64.0 * f64::from(period.max(1)));
        self.noise_phase += frequency / f64::from(sample_rate);
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

fn advance_envelope(
    operator: &mut Operator,
    registers: &[u8],
    offset: usize,
    key_code: u8,
    sample_rate: u32,
) {
    let key_scale = registers[0x80 + offset] >> 6;
    let key_rate = if key_scale == 0 {
        0
    } else {
        (key_code >> (4 - key_scale)).min(31)
    };
    let attack = (registers[0x80 + offset] & 0x1f)
        .saturating_add(key_rate)
        .min(31);
    let decay = (registers[0xa0 + offset] & 0x1f)
        .saturating_add(key_rate)
        .min(31);
    let sustain_rate = (registers[0xc0 + offset] & 0x1f)
        .saturating_add(key_rate)
        .min(31);
    let sustain_level = (registers[0xe0 + offset] >> 4).min(15);
    let release = ((registers[0xe0 + offset] & 0x0f) * 2 + 1)
        .saturating_add(key_rate)
        .min(31);
    let rate = f64::from(sample_rate);
    match operator.stage {
        1 => {
            operator.envelope += (f64::from(attack) + 1.0).powi(2) / (rate * 16.0);
            if operator.envelope >= 1.0 {
                operator.envelope = 1.0;
                operator.stage = 2;
            }
        }
        2 => {
            operator.envelope -= (f64::from(decay) + 1.0).powi(2) / (rate * 256.0);
            let target = 1.0 - f64::from(sustain_level) / 15.0;
            if operator.envelope <= target {
                operator.envelope = target;
                operator.stage = 3;
            }
        }
        3 => {
            operator.envelope = (operator.envelope
                - (f64::from(sustain_rate) + 1.0).powi(2) / (rate * 1024.0))
                .max(0.0)
        }
        4 => {
            operator.envelope =
                (operator.envelope - (f64::from(release) + 1.0).powi(2) / (rate * 512.0)).max(0.0);
            if operator.envelope == 0.0 {
                operator.stage = 0;
            }
        }
        _ => operator.envelope = 0.0,
    }
}

fn operator_detune(registers: &[u8], offset: usize) -> f64 {
    const DT1_CENTS: [f64; 8] = [0.0, 3.4, 6.7, 10.0, 0.0, -3.4, -6.7, -10.0];
    const DT2_CENTS: [f64; 4] = [0.0, 600.0, 781.0, 950.0];
    let dt1 = usize::from(registers[0x40 + offset] >> 4 & 7);
    let dt2 = usize::from(registers[0xc0 + offset] >> 6 & 3);
    2f64.powf((DT1_CENTS[dt1] + DT2_CENTS[dt2]) / 1200.0)
}

fn algorithm_output(algorithm: u8, phase: [f64; 4], level: [f64; 4], feedback: f64) -> (f64, f64) {
    let op = |index: usize, modulation: f64| (phase[index] + modulation * 4.0).sin() * level[index];
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
}

impl AudioSystem {
    pub(crate) fn read_ym(&self, offset: u32) -> u8 {
        self.ym2151.read(offset)
    }

    pub(crate) fn write_ym(&mut self, offset: u32, value: u8) {
        self.ym2151.write(offset, value);
        if offset & 3 == 3 && self.ym2151.address == 0x1b {
            self.msm6258.clock_select = (value >> 5) & 4 | (self.msm6258.pan >> 2) & 3;
        }
    }

    pub(crate) fn read_adpcm(&self, offset: u32) -> u8 {
        self.msm6258.read(offset)
    }

    pub(crate) fn write_adpcm(&mut self, offset: u32, value: u8) {
        self.msm6258.write(offset, value);
    }

    pub(crate) fn set_pan(&mut self, value: u8) {
        self.msm6258.pan = value & 0x0f;
        self.msm6258.clock_select = (self.msm6258.clock_select & 4) | ((value >> 2) & 3);
    }

    pub(crate) fn tick(&mut self, cycles: u32, cpu_clock: u32) -> bool {
        self.ym2151.tick(cycles, cpu_clock)
    }

    pub(crate) fn generate(&mut self, frames: usize, sample_rate: u32, output: &mut Vec<f32>) {
        output.reserve(frames * 2);
        for _ in 0..frames {
            let fm = self.ym2151.sample(sample_rate);
            let adpcm = self.msm6258.sample(sample_rate);
            output.push((fm.0 + adpcm.0 * 0.5).clamp(-1.0, 1.0));
            output.push((fm.1 + adpcm.1 * 0.5).clamp(-1.0, 1.0));
        }
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
}
