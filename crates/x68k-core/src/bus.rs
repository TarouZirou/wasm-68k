//! X68000のメモリマップと周辺デバイスの状態。

use std::collections::BTreeMap;

use m68k::AddressBus;
use m68k::core::memory::{BusFault, BusFaultKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::color;
use crate::devices::audio::AudioSystem;
use crate::devices::crtc::{Crtc, Signal};
use crate::devices::dma::{Dma, Transfer, TransferWidth};
use crate::devices::fdc::Fdc;
use crate::devices::gvram::GraphicVram;
use crate::devices::hdc::Hdc;
use crate::devices::mfp::Mfp;
use crate::devices::midi::MidiBoard;
use crate::devices::ppi::Ppi;
use crate::devices::rtc::Rtc;
use crate::devices::scc::Scc;
use crate::devices::sprite::SpriteBg;
use crate::devices::system_port::SystemPort;
use crate::devices::video::Video;
use crate::media::MediaImage;
use crate::scheduler::{Event, Scheduler};
use crate::{
    DriveId, InputEvent, MAX_SCREEN_HEIGHT, MAX_SCREEN_WIDTH, MachineConfig, MachineModel,
};

const GVRAM_BASE: u32 = 0xc0_0000;
const TVRAM_BASE: u32 = 0xe0_0000;
const CRTC_BASE: u32 = 0xe8_0000;
const VIDEO_BASE: u32 = 0xe8_2000;
const DMA_BASE: u32 = 0xe8_4000;
const AREA_BASE: u32 = 0xe8_6000;
const MFP_BASE: u32 = 0xe8_8000;
const RTC_BASE: u32 = 0xe8_a000;
const PRINTER_BASE: u32 = 0xe8_c000;
const PRINTER_DATA: u32 = PRINTER_BASE + 1;
const PRINTER_STROBE: u32 = PRINTER_BASE + 3;
const SYSTEM_BASE: u32 = 0xe8_e000;
const YM_BASE: u32 = 0xe9_0000;
const ADPCM_BASE: u32 = 0xe9_2000;
const FDC_BASE: u32 = 0xe9_4000;
const HDC_BASE: u32 = 0xe9_6000;
const SCC_BASE: u32 = 0xe9_8000;
const PPI_BASE: u32 = 0xe9_a000;
const IOC_BASE: u32 = 0xe9_c000;
const MIDI_BASE: u32 = 0xea_fa00;
const SPRITE_BASE: u32 = 0xeb_0000;
const SRAM_BASE: u32 = 0xed_0000;
const RESET_SUPERVISOR_LIMIT: u32 = 0x2000;
const CGROM_BASE: u32 = 0xf0_0000;
const CGROM_SIZE: u32 = 0x0c_0000;

const TVRAM_SIZE: usize = 0x08_0000;
const SRAM_SIZE: usize = 0x4000;

fn blank_scanout() -> Vec<u16> {
    vec![0; (MAX_SCREEN_WIDTH * MAX_SCREEN_HEIGHT) as usize]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Devices {
    crtc: Crtc,
    video: Video,
    dma: Dma,
    mfp: Mfp,
    rtc: Rtc,
    printer: [u8; 2],
    system: SystemPort,
    audio: AudioSystem,
    fdc: Fdc,
    hdc: Hdc,
    scc: Scc,
    ppi: Ppi,
    ioc: Vec<u8>,
    /// IOCへ入力されている現在の信号（上位nibble）。
    #[serde(default)]
    ioc_signal: u8,
    /// IOCが立ち上がりedgeでlatchedした割り込み要求（下位nibble）。
    #[serde(default)]
    ioc_request: u8,
    #[serde(default)]
    ioc_ack_count: u64,
    #[serde(default)]
    ioc_spurious_ack_count: u64,
    midi: MidiBoard,
    irq_level: u8,
    irq_vector: u8,
}

impl Default for Devices {
    fn default() -> Self {
        Self::new(MachineModel::X68000)
    }
}

impl Devices {
    fn new(model: MachineModel) -> Self {
        let ioc = vec![0; 0x2000];
        Self {
            crtc: Crtc::default(),
            video: Video::default(),
            dma: Dma::new(model),
            mfp: Mfp::default(),
            rtc: Rtc::default(),
            printer: [0, 1],
            system: SystemPort::new(model),
            audio: AudioSystem::default(),
            fdc: Fdc::default(),
            hdc: Hdc::default(),
            scc: Scc::default(),
            ppi: Ppi::default(),
            ioc,
            ioc_signal: 0,
            ioc_request: 0,
            ioc_ack_count: 0,
            ioc_spurious_ack_count: 0,
            midi: MidiBoard::default(),
            irq_level: 0,
            irq_vector: 0xff,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Bus {
    pub model: MachineModel,
    pub clock_hz: u32,
    pub sample_rate: u32,
    pub ram: Vec<u8>,
    pub gvram: GraphicVram,
    pub tvram: Vec<u8>,
    pub sprite_ram: SpriteBg,
    pub sram: Vec<u8>,
    #[serde(skip, default)]
    pub ipl: Vec<u8>,
    #[serde(skip, default)]
    pub cgrom: Vec<u8>,
    #[serde(skip, default)]
    pub scsi_rom: Vec<u8>,
    pub reset_overlay: bool,
    supervisor: bool,
    supervisor_limit: u32,
    pub sram_write_enabled: bool,
    pub devices: Devices,
    scheduler: Scheduler,
    /// CRTCが実際に通過した走査線だけを保持する。XSPのsprite doublerは
    /// raster IRQ中に同じsprite番号を再配置するため、frame末尾のRAM状態から
    /// 全画面を再構築することはできない。
    #[serde(skip, default = "blank_scanout")]
    scanout_frame: Vec<u16>,
    #[serde(skip, default)]
    scanout_width: u32,
    #[serde(skip, default)]
    scanout_height: u32,
    #[serde(skip, default)]
    frame_boundary_pending: bool,
    /// deserialize直後など、可視領域の途中から始まった不完全なframeを公開しない。
    #[serde(skip, default)]
    scanout_started: bool,
    accumulated_wait: u32,
    first_fault_address: Option<u32>,
    last_fault_address: Option<u32>,
    fault_count: u64,
    pub media: BTreeMap<DriveId, MediaImage>,
}

impl Bus {
    pub fn new(config: &MachineConfig, ram_bytes: usize) -> Self {
        Self {
            model: config.model,
            clock_hz: config.model.clock_hz(),
            sample_rate: config.sample_rate,
            ram: vec![0; ram_bytes],
            gvram: GraphicVram::default(),
            tvram: vec![0; TVRAM_SIZE],
            sprite_ram: SpriteBg::default(),
            // 初期化済みSRAMの予約領域は0。特にX68030 IPLは例外vectorを
            // 構築する前に$ED0091（起動音）を参照するため、消去状態の0xffを
            // そのまま公開すると未初期化TRAP #15へ入りstackを破壊する。
            sram: vec![0; SRAM_SIZE],
            ipl: Vec::new(),
            cgrom: Vec::new(),
            scsi_rom: Vec::new(),
            reset_overlay: true,
            supervisor: true,
            supervisor_limit: RESET_SUPERVISOR_LIMIT,
            sram_write_enabled: false,
            devices: Devices::new(config.model),
            scheduler: Scheduler::new(config.model.clock_hz()),
            scanout_frame: blank_scanout(),
            scanout_width: 768,
            scanout_height: 512,
            frame_boundary_pending: false,
            scanout_started: false,
            accumulated_wait: 0,
            first_fault_address: None,
            last_fault_address: None,
            fault_count: 0,
            media: BTreeMap::new(),
        }
    }

    pub fn reset(&mut self) {
        self.reset_overlay = true;
        self.supervisor = true;
        self.supervisor_limit = RESET_SUPERVISOR_LIMIT;
        self.sram_write_enabled = false;
        self.devices = Devices::new(self.model);
        self.scheduler.reset();
        self.scanout_frame.fill(0);
        self.scanout_width = 768;
        self.scanout_height = 512;
        self.frame_boundary_pending = false;
        self.scanout_started = false;
        self.accumulated_wait = 0;
        self.first_fault_address = None;
        self.last_fault_address = None;
        self.fault_count = 0;
    }

    pub fn release_reset_overlay(&mut self) {
        self.reset_overlay = false;
    }

    pub fn set_supervisor(&mut self, supervisor: bool) {
        self.supervisor = supervisor;
    }

    fn record_bus_fault(&mut self, address: u32) -> BusFault {
        let address = self.mask_address(address);
        self.first_fault_address.get_or_insert(address);
        self.last_fault_address = Some(address);
        self.fault_count = self.fault_count.wrapping_add(1);
        fault(address)
    }

    pub fn pending_irq(&mut self) -> u8 {
        self.refresh_ioc_requests();
        let mfp = if self.devices.mfp.has_interrupt() {
            6
        } else {
            0
        };
        let dma = if self.devices.dma.interrupt_pending() {
            3
        } else {
            0
        };
        let fdc = if self.devices.ioc_request & 0x06 != 0 {
            1
        } else {
            0
        };
        let hdc = if self.devices.ioc_request & 0x08 != 0 {
            1
        } else {
            0
        };
        let midi = if self.devices.midi.interrupt_pending() {
            4
        } else {
            0
        };
        let scc = if self.devices.scc.interrupt_pending() {
            5
        } else {
            0
        };
        self.devices
            .irq_level
            .max(mfp)
            .max(dma)
            .max(fdc)
            .max(hdc)
            .max(midi)
            .max(scc)
    }

    fn live_ioc_signal(&self) -> u8 {
        (if self.devices.fdc.interrupt_pending() {
            0x80
        } else {
            0
        }) | (if self.devices.fdc.media_interrupt_pending() {
            0x40
        } else {
            0
        }) | (if self.devices.hdc.interrupt_pending() {
            0x10
        } else {
            0
        })
    }

    fn requests_for_signal(signal: u8, enable: u8) -> u8 {
        (if signal & 0x80 != 0 && enable & 0x04 != 0 {
            0x04
        } else {
            0
        }) | (if signal & 0x40 != 0 && enable & 0x02 != 0 {
            0x02
        } else {
            0
        }) | (if signal & 0x10 != 0 && enable & 0x08 != 0 {
            0x08
        } else {
            0
        })
    }

    /// IOCはデバイスのIRQ levelをMPUへ直結せず、0→1 edgeを要求として保持する。
    /// CPU acknowledgeで要求だけが消え、デバイス信号が一度下がるまでは再要求しない。
    fn refresh_ioc_requests(&mut self) {
        let signal = self.live_ioc_signal();
        let rising = signal & !self.devices.ioc_signal;
        self.devices.ioc_request |= Self::requests_for_signal(rising, self.devices.ioc[1] & 0x0f);
        self.devices.ioc_signal = signal;
    }

    pub fn tick(&mut self, cycles: u32) {
        self.devices.mfp.tick(cycles, self.clock_hz);
        if self.devices.rtc.tick(cycles, self.clock_hz) {
            self.devices.mfp.raise(15);
        }
        if self.devices.audio.tick(cycles, self.clock_hz) {
            self.devices.mfp.raise(12);
        }
        self.devices.midi.tick(cycles, self.clock_hz);
        for event in self.scheduler.advance(cycles) {
            match event {
                // CPUは走査線開始時のraster IRQでsprite tableを書き換える。
                // 有効表示が終わるHSYNC直前に画素を確定すれば、EB0808 bit 9が
                // CPUアクセス側へ落ちている途中の状態を誤って表示しない。
                Event::HorizontalSync => {
                    for line in self
                        .devices
                        .crtc
                        .visible_output_lines()
                        .into_iter()
                        .flatten()
                    {
                        if self.scanout_started && line < self.scanout_height {
                            self.render_scanout_line(line);
                        }
                    }
                }
                Event::Scanline => {
                    let signals = self.devices.crtc.next_scanline();
                    if self.devices.crtc.at_visible_start() {
                        let (width, height) = self.screen_dimensions();
                        self.scanout_width = width.clamp(1, MAX_SCREEN_WIDTH);
                        self.scanout_height = height.clamp(1, MAX_SCREEN_HEIGHT);
                        self.scanout_frame[..(self.scanout_width * self.scanout_height) as usize]
                            .fill(0);
                        self.scanout_started = true;
                    }
                    for signal in signals {
                        if signal == Signal::FrameBoundary {
                            if self.scanout_started {
                                self.frame_boundary_pending = true;
                                self.scanout_started = false;
                            }
                            continue;
                        }
                        let vertical_edge = match signal {
                            Signal::VerticalDisplayStart => Some(true),
                            Signal::VerticalDisplayEnd => Some(false),
                            _ => None,
                        };
                        if let Some(rising) = vertical_edge {
                            // GPIP4はV-DISPのedge入力。AERと逆側のedgeまで
                            // source 9へ送ると、VBlank更新が可視領域でも再実行される。
                            if self.devices.mfp.gpip_rising_edge(0x10) == rising {
                                self.devices.mfp.timer_a_event();
                                self.devices.mfp.raise(9);
                            }
                            continue;
                        }
                        self.devices.mfp.raise(match signal {
                            Signal::HorizontalSync => 0,
                            Signal::Raster => 1,
                            Signal::VerticalDisplayStart | Signal::VerticalDisplayEnd => {
                                unreachable!()
                            }
                            Signal::FrameBoundary => unreachable!(),
                        });
                    }
                }
            }
        }
        let budget = (cycles / 4).clamp(1, 4096);
        for _ in 0..budget {
            let mut progressed = false;
            for channel in 0..4 {
                if let Some(transfer) = self.devices.dma.next_transfer(channel) {
                    // HD63450は周辺機器のDREQがassertされるまで外部要求転送を
                    // 開始しない。IPLはDMACをFDCコマンドより先にstartするため、
                    // ここを無条件に進めると空FIFOでTerminal Countへ到達してしまう。
                    if !self.dma_transfer_ready(transfer) {
                        continue;
                    }
                    let success = self.execute_dma_transfer(transfer);
                    let terminal_count = self.devices.dma.complete(transfer, success);
                    if terminal_count
                        && success
                        && (transfer.source == FDC_BASE + 3 || transfer.destination == FDC_BASE + 3)
                    {
                        self.devices.fdc.terminal_count();
                        self.refresh_ioc_requests();
                    }
                    self.service_dma_chain(channel);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
    }

    fn dma_transfer_ready(&self, transfer: Transfer) -> bool {
        if transfer.source == FDC_BASE + 3 {
            return self.devices.fdc.dma_read_ready();
        }
        if transfer.destination == FDC_BASE + 3 {
            return self.devices.fdc.dma_write_ready();
        }
        true
    }

    pub fn input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key { scancode, pressed } => {
                if self.devices.system.keyboard_enabled() {
                    let value = if pressed { scancode } else { scancode | 0x80 };
                    self.devices.mfp.receive_keyboard(value);
                }
            }
            InputEvent::MouseMove { dx, dy } => {
                self.devices.scc.move_mouse(dx, dy);
            }
            InputEvent::MouseButton { button, pressed } => {
                self.devices.scc.set_button(button, pressed);
            }
            InputEvent::JoystickButton {
                port,
                button,
                pressed,
            } => {
                self.devices.ppi.set_button(port, button, pressed);
            }
            InputEvent::JoystickAxis { port, axis, value } => {
                self.devices.ppi.set_axis(port, axis, value);
            }
        }
    }

    pub fn notify_media_change(&mut self, drive: DriveId) {
        if let DriveId::Floppy(number) = drive {
            self.refresh_ioc_requests();
            self.devices.fdc.notify_media_change(number);
            self.refresh_ioc_requests();
        }
    }

    pub fn render_frame(&self, frame: &mut [u16], width: u32, height: u32, _frame_no: u64) {
        if self.ipl.is_empty() {
            // Phase 0のテストパターンはコアから撤去済み。ROM未読込時は実機同様、
            // CPUを動かさず黒画面を返す。公開自己診断は通常のIPLとして実行する。
            frame.fill(0);
            return;
        }
        for y in 0..height {
            let start = (y * width) as usize;
            self.render_line(&mut frame[start..start + width as usize], width, y);
        }
    }

    /// CRTCが確定した最新のscanoutをMachine側へ渡す。hostの固定60Hzとは
    /// 独立しており、映像frameが未完成なら前回のframebufferを保持する。
    pub fn take_scanout(&mut self, frame: &mut [u16]) -> Option<(u32, u32)> {
        if !std::mem::take(&mut self.frame_boundary_pending) {
            return None;
        }
        let length = (self.scanout_width * self.scanout_height) as usize;
        frame[..length].copy_from_slice(&self.scanout_frame[..length]);
        Some((self.scanout_width, self.scanout_height))
    }

    fn render_scanout_line(&mut self, y: u32) {
        let width = self.scanout_width;
        let mut line = [0u16; MAX_SCREEN_WIDTH as usize];
        self.render_line(&mut line[..width as usize], width, y);
        let start = (y * width) as usize;
        self.scanout_frame[start..start + width as usize].copy_from_slice(&line[..width as usize]);
    }

    fn render_line(&self, line: &mut [u16], width: u32, y: u32) {
        let (text_scroll_x, text_scroll_y) = self.devices.crtc.text_scroll();
        let scrolls = self.devices.crtc.graphic_scrolls();
        let mut sprite_line = [0u32; MAX_SCREEN_WIDTH as usize];
        if self.devices.video.sprites_enabled() {
            self.sprite_ram.render_scanline(
                &self.devices.video,
                width,
                y,
                &mut sprite_line[..width as usize],
            );
        }
        for x in 0..width {
            let pixel = x as usize;
            let graphic = self
                .devices
                .video
                .graphics_enabled()
                .then(|| {
                    self.devices
                        .video
                        .graphic_pixel_with_attributes(&self.gvram, scrolls, x, y)
                })
                .flatten();
            let text_x = (x + u32::from(text_scroll_x)) & 1023;
            let text_y = (y + u32::from(text_scroll_y)) & 1023;
            let byte_in_line = (text_y as usize * 128) + text_x as usize / 8;
            let bit = 7 - (text_x as usize & 7);
            let mut text_color = 0u8;
            for plane in 0..4 {
                let index = plane * 0x20_000 + byte_in_line;
                if index < self.tvram.len() {
                    text_color |= ((self.tvram[index] >> bit) & 1) << plane;
                }
            }
            let text = (text_color != 0 && self.devices.video.text_enabled())
                .then(|| self.devices.video.text_colour(text_color));
            let sprite = (sprite_line[pixel] & 0x1_0000 != 0).then_some(sprite_line[pixel] as u16);
            // priority値は0が最前面、2/3が最背面。同値時は
            // graphics < sprite/BG < text の順で前面になる。
            // 固定長配列を使い、走査線のhot pathでallocationしない。
            let mut layers = [(0u8, 0u8, 0u16, false, false); 3];
            let mut layer_count = 0usize;
            if let Some((value, special)) = graphic {
                let priority = if special && self.devices.video.special_priority_enabled() {
                    0
                } else {
                    self.devices.video.layer_priority(0)
                };
                let tie = if special && self.devices.video.special_priority_enabled() {
                    3
                } else {
                    0
                };
                layers[layer_count] = (priority, tie, value, true, special);
                layer_count += 1;
            }
            if let Some(value) = sprite {
                layers[layer_count] =
                    (self.devices.video.layer_priority(2), 1, value, false, false);
                layer_count += 1;
            }
            if let Some(value) = text {
                layers[layer_count] =
                    (self.devices.video.layer_priority(1), 2, value, false, false);
                layer_count += 1;
            }
            layers[..layer_count]
                .sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
            line[pixel] = if self.devices.video.half_transparency_enabled()
                && layer_count >= 2
                && layers[layer_count - 1].3
                && layers[layer_count - 1].4
            {
                color::blend_half(layers[layer_count - 1].2, layers[layer_count - 2].2)
            } else {
                layer_count
                    .checked_sub(1)
                    .map_or(0, |index| layers[index].2)
            };
        }
    }

    pub fn generate_audio(&mut self, frames: usize, output: &mut Vec<f32>) {
        self.devices
            .audio
            .generate(frames, self.sample_rate, output);
    }

    pub fn drain_midi(&mut self) -> Vec<u8> {
        self.devices.midi.drain()
    }

    pub fn screen_dimensions(&self) -> (u32, u32) {
        self.devices.crtc.dimensions()
    }

    pub fn take_wait_cycles(&mut self) -> u32 {
        std::mem::take(&mut self.accumulated_wait)
    }

    pub fn fault_diagnostics(&self) -> (Option<u32>, Option<u32>, u64) {
        (
            self.first_fault_address,
            self.last_fault_address,
            self.fault_count,
        )
    }

    pub fn fdc_diagnostics(&self) -> (u64, u64, u8, u8, usize) {
        self.devices.fdc.diagnostics()
    }

    pub fn fdc_result_status(&self) -> [u8; 3] {
        self.devices.fdc.result_status()
    }

    pub fn fdc_command_parameters(&self) -> [u8; 8] {
        self.devices.fdc.command_parameters()
    }

    pub fn dma_diagnostics(&self, channel: usize) -> (u8, u8, u8, u8, u16, u32, u32) {
        let base = (channel.min(3) as u32) << 6;
        let read_u16 = |high: u32, low: u32| {
            u16::from_be_bytes([
                self.devices.dma.read(base + high),
                self.devices.dma.read(base + low),
            ])
        };
        let read_u32 = |start: u32| {
            u32::from_be_bytes(std::array::from_fn(|index| {
                self.devices.dma.read(base + start + index as u32)
            }))
        };
        (
            self.devices.dma.read(base),
            self.devices.dma.read(base + 1),
            self.devices.dma.read(base + 5),
            self.devices.dma.read(base + 7),
            read_u16(0x0a, 0x0b),
            read_u32(0x0c),
            read_u32(0x14),
        )
    }

    pub fn ram_diagnostics(&self, address: u32, length: usize) -> Vec<u8> {
        let start = address as usize;
        let end = start.saturating_add(length.min(4096)).min(self.ram.len());
        self.ram.get(start..end).unwrap_or_default().to_vec()
    }

    pub fn sprite_diagnostics(&self) -> Vec<(u8, u16, u16, u16, u8)> {
        self.sprite_ram.diagnostics()
    }

    pub fn ioc_diagnostics(&self) -> (u8, u8, u8, u8, u32, u64, u64) {
        let vector = self.devices.ioc[3];
        let offset = usize::from(vector) * 4;
        let handler = self
            .ram
            .get(offset..offset + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_be_bytes)
            .unwrap_or(0);
        (
            self.live_ioc_signal(),
            self.devices.ioc_request,
            self.devices.ioc[1] & 0x0f,
            vector,
            handler,
            self.devices.ioc_ack_count,
            self.devices.ioc_spurious_ack_count,
        )
    }

    pub fn cycles_until_next_event(&self) -> u32 {
        if self.devices.mfp.needs_scanline_boundaries() {
            self.scheduler.cycles_until_next_event()
        } else {
            4096
        }
    }

    fn execute_dma_transfer(&mut self, transfer: Transfer) -> bool {
        match transfer.width {
            TransferWidth::Byte => self
                .try_read_byte(transfer.source)
                .and_then(|value| self.try_write_byte(transfer.destination, value)),
            TransferWidth::Word => self
                .try_read_word(transfer.source)
                .and_then(|value| self.try_write_word(transfer.destination, value)),
            TransferWidth::Long => self
                .try_read_long(transfer.source)
                .and_then(|value| self.try_write_long(transfer.destination, value)),
        }
        .is_ok()
    }

    fn service_dma_chain(&mut self, channel: usize) {
        let Some((address, link)) = self.devices.dma.chain_descriptor_request(channel) else {
            return;
        };
        let descriptor = self
            .try_read_long(address)
            .and_then(|mar| {
                self.try_read_word(address.wrapping_add(4))
                    .map(|mtc| (mar, mtc))
            })
            .and_then(|(mar, mtc)| {
                if link {
                    self.try_read_long(address.wrapping_add(6))
                        .map(|next| (mar, mtc, Some(next)))
                } else {
                    Ok((mar, mtc, None))
                }
            });
        match descriptor {
            Ok((mar, mtc, next)) => self
                .devices
                .dma
                .load_chain_descriptor(channel, mar, mtc, next),
            Err(_) => self.devices.dma.chain_failed(channel),
        }
    }

    pub fn content_hashes(&self) -> Vec<(String, [u8; 32])> {
        let mut hashes = Vec::new();
        for (name, bytes) in [
            ("rom:ipl", self.ipl.as_slice()),
            ("rom:cgrom", self.cgrom.as_slice()),
            ("rom:scsi", self.scsi_rom.as_slice()),
        ] {
            if !bytes.is_empty() {
                hashes.push((name.to_string(), Sha256::digest(bytes).into()));
            }
        }
        for (drive, image) in &self.media {
            let name = match drive {
                DriveId::Floppy(number) => format!("fdd:{number}"),
                DriveId::HardDisk(number) => format!("hdd:{number}"),
            };
            hashes.push((name, image.digest()));
        }
        hashes
    }

    pub fn reattach_immutable(&mut self, current: &Self) -> bool {
        if self.model != current.model || self.media.len() != current.media.len() {
            return false;
        }
        self.ipl.clone_from(&current.ipl);
        self.cgrom.clone_from(&current.cgrom);
        self.scsi_rom.clone_from(&current.scsi_rom);
        for (drive, media) in &mut self.media {
            let Some(current_media) = current.media.get(drive) else {
                return false;
            };
            if !media.reattach_original(current_media) {
                return false;
            }
        }
        true
    }

    fn read_mapped(&mut self, address: u32) -> Option<u8> {
        let address = self.mask_address(address);
        self.record_memory_wait(address);
        if !self.supervisor && address < self.supervisor_limit {
            return None;
        }
        if self.reset_overlay && address < 8 && !self.ipl.is_empty() {
            let vector_base = self.ipl.len().saturating_sub(0x1_0000);
            return self.ipl.get(vector_base + address as usize).copied();
        }
        if let Some(value) = get_range(&self.ram, 0, address) {
            return Some(value);
        }
        if (GVRAM_BASE..GVRAM_BASE + 0x20_0000).contains(&address) {
            return Some(
                self.gvram
                    .read(address - GVRAM_BASE, self.devices.crtc.memory_mode()),
            );
        }
        if let Some(value) = get_range(&self.tvram, TVRAM_BASE, address) {
            return Some(value);
        }
        if (SPRITE_BASE..SPRITE_BASE + 0x1_0000).contains(&address) {
            return Some(self.sprite_ram.read(address - SPRITE_BASE));
        }
        // SRAM $ED0008.w はIPL/Human68kが参照する搭載RAM容量（64KiB単位）。
        // 実SRAM dumpの初期値に依存させると0xffffを巨大なRAMとして解釈し、
        // 実在しない領域へメモリ管理情報を書いて実行不能になる。
        if address == SRAM_BASE + 8 {
            return Some(((self.ram.len() >> 16) as u16).to_be_bytes()[0]);
        }
        if address == SRAM_BASE + 9 {
            return Some(((self.ram.len() >> 16) as u16).to_be_bytes()[1]);
        }
        if let Some(value) = get_range(&self.sram, SRAM_BASE, address) {
            return Some(value);
        }
        if (CGROM_BASE..CGROM_BASE + CGROM_SIZE).contains(&address) {
            // 実機ではCGROMソケットのアドレス領域が常に存在する。合法なdumpが
            // 未読込でもbus errorにはせず、空フォントとして0を返す。これにより
            // IPLの文字描画呼出しが例外暴走せず、後から実CGROMへ差し替えられる。
            return Some(
                self.cgrom
                    .get((address - CGROM_BASE) as usize)
                    .copied()
                    .unwrap_or(0),
            );
        }
        let scsi_base = self.scsi_rom_base();
        if (self.scsi_rom.is_empty() || scsi_base != 0x00fc_0000)
            && (0x00fc_0000..0x00fe_0000).contains(&address)
        {
            // 内蔵SCSI未搭載機でもIPL/Human68kと一部ゲームは$FC0000窓を
            // 存在確認する。ROMが無い場合はdata bus pull-upとして応答する。
            return Some(0xff);
        }
        if let Some(value) = get_range(&self.scsi_rom, scsi_base, address) {
            return Some(value);
        }
        // IPLはアドレス空間の末尾へ配置する（128KiBならFE0000、256KiBならFC0000）。
        let ipl_base = 0x0100_0000u32.saturating_sub(self.ipl.len() as u32);
        if let Some(value) = get_range(&self.ipl, ipl_base, address) {
            return Some(value);
        }
        if self.model == MachineModel::X68030 && address >= 0x0100_0000 {
            // 030 IPL/Human68kの拡張RAM走査。未搭載の32bit空間はwriteが
            // retainされないopen busとして見せ、比較による容量判定を行わせる。
            return Some(0xff);
        }
        self.read_io(address)
    }

    fn write_mapped(&mut self, address: u32, value: u8) -> bool {
        let address = self.mask_address(address);
        self.record_memory_wait(address);
        if !self.supervisor && address < self.supervisor_limit {
            return false;
        }
        if set_range(&mut self.ram, 0, address, value) {
            return true;
        }
        if (TVRAM_BASE..TVRAM_BASE + TVRAM_SIZE as u32).contains(&address) {
            self.devices
                .crtc
                .write_tvram(address - TVRAM_BASE, value, &mut self.tvram);
            return true;
        }
        if (SPRITE_BASE..SPRITE_BASE + 0x1_0000).contains(&address) {
            self.sprite_ram.write(address - SPRITE_BASE, value);
            return true;
        }
        if (GVRAM_BASE..GVRAM_BASE + 0x20_0000).contains(&address) {
            self.gvram
                .write(address - GVRAM_BASE, value, self.devices.crtc.memory_mode());
            return true;
        }
        if address >= SRAM_BASE && address < SRAM_BASE + SRAM_SIZE as u32 {
            if self.sram_write_enabled {
                self.sram[(address - SRAM_BASE) as usize] = value;
            }
            return true;
        }
        let scsi_base = self.scsi_rom_base();
        let ipl_base = 0x0100_0000u32.saturating_sub(self.ipl.len() as u32);
        if (CGROM_BASE..CGROM_BASE + CGROM_SIZE).contains(&address)
            || (!self.scsi_rom.is_empty()
                && (scsi_base..scsi_base + self.scsi_rom.len() as u32).contains(&address))
            || (0x00fc_0000..0x00fe_0000).contains(&address)
            || (!self.ipl.is_empty()
                && (ipl_base..ipl_base + self.ipl.len() as u32).contains(&address))
        {
            // ROMは書換わらないがバスcycle自体には応答する。no-op writeを
            // bus errorにするとX68030 IPLのROM/protection probeが例外暴走する。
            return true;
        }
        if self.model == MachineModel::X68030 && address >= 0x0100_0000 {
            return true;
        }
        self.write_io(address, value)
    }

    fn mask_address(&self, address: u32) -> u32 {
        match self.model {
            // M68EC030のレジスタ／拡張空間は32-bitのまま保持する一方、
            // 68000互換コードが符号拡張した0xFFxx_xxxxはオンボードの
            // 24-bit legacy windowへdecodeされる。
            MachineModel::X68030 if address >= 0xff00_0000 => address & 0x00ff_ffff,
            MachineModel::X68030 => address,
            _ => address & 0x00ff_ffff,
        }
    }

    fn scsi_rom_base(&self) -> u32 {
        // 8 KiB expansion ROMs are decoded at $EA0000 on X68000/XVI. The
        // X68030's internal 8 KiB ROM and all 128 KiB images use $FC0000.
        if self.scsi_rom.len() == 0x20_000 || self.model == MachineModel::X68030 {
            0x00fc_0000
        } else {
            0x00ea_0000
        }
    }

    fn record_memory_wait(&mut self, address: u32) {
        if self.model != MachineModel::X68030 {
            return;
        }
        let reset_rom = self.reset_overlay && address < 8 && !self.ipl.is_empty();
        let ipl_base = 0x0100_0000u32.saturating_sub(self.ipl.len() as u32);
        let rom = reset_rom
            || (!self.ipl.is_empty()
                && (ipl_base..ipl_base.saturating_add(self.ipl.len() as u32)).contains(&address))
            || (!self.cgrom.is_empty()
                && (CGROM_BASE..CGROM_BASE + self.cgrom.len() as u32).contains(&address))
            || (!self.scsi_rom.is_empty()
                && (self.scsi_rom_base()
                    ..self
                        .scsi_rom_base()
                        .saturating_add(self.scsi_rom.len() as u32))
                    .contains(&address));
        if rom {
            self.accumulated_wait = self
                .accumulated_wait
                .saturating_add(self.devices.system.memory_wait(true));
        } else if (address as usize) < self.ram.len() {
            self.accumulated_wait = self
                .accumulated_wait
                .saturating_add(self.devices.system.memory_wait(false));
        }
    }

    fn read_io(&mut self, address: u32) -> Option<u8> {
        if let Some((offset, spc)) = self.hdc_register_offset(address) {
            return Some(if spc {
                self.devices.hdc.read_spc(offset, &self.media)
            } else {
                self.devices.hdc.read(offset, &self.media)
            });
        }
        if self.is_disconnected_legacy_hdc(address) {
            // XVI/030のIPLは初代SASI窓を存在確認する。未接続窓をCPUの
            // bus errorへ送ると、復帰frame未実装の命令でstackを壊すため、
            // data bus pull-up相当のopen busとして応答する。
            return Some(0xff);
        }
        match address {
            CRTC_BASE..=0xe8_1fff => {
                return Some(self.devices.crtc.read(address - CRTC_BASE));
            }
            VIDEO_BASE..=0xe8_3fff => {
                return Some(self.devices.video.read(address - VIDEO_BASE));
            }
            DMA_BASE..=0xe8_5fff => {
                return Some(self.devices.dma.read(address - DMA_BASE));
            }
            AREA_BASE..=0xe8_7fff => return Some(0xff),
            MFP_BASE..=0xe8_9fff => {
                return Some(
                    self.devices.mfp.read(
                        address - MFP_BASE,
                        self.devices
                            .crtc
                            .gpip(self.scheduler.horizontal_sync_high()),
                    ),
                );
            }
            RTC_BASE..=0xe8_bfff => {
                return Some(self.devices.rtc.read(address - RTC_BASE));
            }
            SYSTEM_BASE..=0xe8_ffff => {
                return Some(self.devices.system.read(address - SYSTEM_BASE));
            }
            YM_BASE..=0xe9_1fff => {
                return Some(self.devices.audio.read_ym(address - YM_BASE));
            }
            ADPCM_BASE..=0xe9_3fff => {
                return Some(self.devices.audio.read_adpcm(address - ADPCM_BASE));
            }
            FDC_BASE..=0xe9_5fff => {
                let value = self.devices.fdc.read(address - FDC_BASE, &self.media);
                // FDCのINTは結果byteの読み出しでも変化する。次のcommandが同じ
                // CPU slice内で開始される前にfallをIOCへ伝え、次のriseを失わない。
                self.refresh_ioc_requests();
                return Some(value);
            }
            HDC_BASE..=0xe9_7fff => return None,
            SCC_BASE..=0xe9_9fff => {
                return Some(self.devices.scc.read(address - SCC_BASE));
            }
            PPI_BASE..=0xe9_bfff => {
                return Some(self.devices.ppi.read(address - PPI_BASE));
            }
            IOC_BASE..=0xe9_dfff => {
                return Some(if address - IOC_BASE == 1 {
                    self.refresh_ioc_requests();
                    self.ioc_status()
                } else {
                    0xff
                });
            }
            MIDI_BASE..=0xea_faff => {
                return Some(self.devices.midi.read(address - MIDI_BASE));
            }
            0xea_0000..=0xea_ffff if self.model == MachineModel::X68030 => {
                // X68030 IPLは拡張I/O空間を走査する。未装着slotはpull-upされた
                // data busとして扱い、未実装の68030 format-A bus-error frameへ
                // 入って起動状態を壊さないようにする。
                return Some(0xff);
            }
            _ => return None,
        }
    }

    fn write_io(&mut self, address: u32, value: u8) -> bool {
        if (YM_BASE..=YM_BASE + 0x1fff).contains(&address) {
            self.devices.audio.write_ym(address - YM_BASE, value);
            return true;
        }
        if (ADPCM_BASE..=ADPCM_BASE + 0x1fff).contains(&address) {
            self.devices.audio.write_adpcm(address - ADPCM_BASE, value);
            return true;
        }
        if (MIDI_BASE..=MIDI_BASE + 0xff).contains(&address) {
            self.devices.midi.write(address - MIDI_BASE, value);
            return true;
        }
        if let Some((offset, spc)) = self.hdc_register_offset(address) {
            if spc {
                self.devices.hdc.write_spc(offset, value, &mut self.media);
            } else {
                self.devices.hdc.write(offset, value, &mut self.media);
            }
            return true;
        }
        if self.is_disconnected_legacy_hdc(address) {
            return true;
        }
        match address {
            CRTC_BASE..=0xe8_1fff => {
                let fast_clear =
                    self.devices
                        .crtc
                        .write(address - CRTC_BASE, value, &mut self.tvram);
                if let Some(mask) = fast_clear {
                    let scroll = self.devices.crtc.graphic_scrolls()[0];
                    let (width, height) = self.devices.crtc.fast_clear_dimensions();
                    self.gvram.fast_clear(mask, scroll, width, height);
                }
                self.scheduler.set_video_timing(
                    self.devices.crtc.v_total(),
                    self.devices.crtc.high_resolution(),
                );
                return true;
            }
            VIDEO_BASE..=0xe8_3fff => {
                self.devices.video.write(address - VIDEO_BASE, value);
                return true;
            }
            DMA_BASE..=0xe8_5fff => {
                self.devices.dma.write(address - DMA_BASE, value);
                if address & 0x3f == 7 && value & 0x80 != 0 {
                    let channel = ((address - DMA_BASE) >> 6 & 3) as usize;
                    self.service_dma_chain(channel);
                }
                return true;
            }
            AREA_BASE..=0xe8_7fff => {
                // $000000から((value + 1) * 8KiB - 1)までをsupervisor領域にする。
                // 0でも先頭8KiBが保護され、$ffで最大2MiBとなる。
                self.supervisor_limit = (u32::from(value) + 1) * 0x2000;
                return true;
            }
            MFP_BASE..=0xe8_9fff => {
                self.devices.mfp.write(address - MFP_BASE, value);
                return true;
            }
            RTC_BASE..=0xe8_bfff => {
                self.devices.rtc.write(address - RTC_BASE, value);
                return true;
            }
            PRINTER_DATA => {
                self.devices.printer[0] = value;
                return true;
            }
            PRINTER_STROBE => {
                self.devices.printer[1] = value & 1;
                return true;
            }
            SYSTEM_BASE..=0xe8_ffff => {
                if let Some(enabled) = self.devices.system.write(address - SYSTEM_BASE, value) {
                    self.sram_write_enabled = enabled;
                }
                return true;
            }
            FDC_BASE..=0xe9_5fff => {
                self.devices
                    .fdc
                    .write(address - FDC_BASE, value, &mut self.media);
                self.refresh_ioc_requests();
                return true;
            }
            HDC_BASE..=0xe9_7fff => return false,
            SCC_BASE..=0xe9_9fff => {
                self.devices.scc.write(address - SCC_BASE, value);
                return true;
            }
            PPI_BASE..=0xe9_bfff => {
                self.devices.ppi.write(address - PPI_BASE, value);
                self.devices.audio.set_pan(self.devices.ppi.port_c());
                return true;
            }
            IOC_BASE..=0xe9_dfff => {
                match address - IOC_BASE {
                    1 => {
                        self.refresh_ioc_requests();
                        let previous = self.devices.ioc[1] & 0x0f;
                        let enabled = value & 0x0f;
                        self.devices.ioc[1] = enabled;
                        // disableで未処理要求を取り下げ、既にhighの信号をenable
                        // した場合はその場で要求をlatchedする。
                        self.devices.ioc_request &= enabled;
                        let newly_enabled = enabled & !previous;
                        self.devices.ioc_request |=
                            Self::requests_for_signal(self.devices.ioc_signal, newly_enabled);
                    }
                    3 => self.devices.ioc[3] = value & 0xfc,
                    _ => {}
                }
                return true;
            }
            0xea_0000..=0xea_ffff if self.model == MachineModel::X68030 => return true,
            _ => return false,
        }
    }

    fn ioc_status(&self) -> u8 {
        // bit 5はプリンタreadyとして常時1。low nibbleは割り込みenable。
        self.live_ioc_signal() | 0x20 | (self.devices.ioc[1] & 0x0f)
    }

    fn hdc_register_offset(&self, address: u32) -> Option<(u32, bool)> {
        let raw = address.checked_sub(HDC_BASE)?;
        match self.model {
            // 初代SASIは先頭32byte。8KiB拡張SCSI ROMを装着した場合は
            // MB89352互換側（+0x20）も公開する。
            MachineModel::X68000 if raw <= 0x1f => Some((raw, false)),
            MachineModel::X68000
                if self.scsi_rom.len() == 0x2000 && (0x20..=0x3f).contains(&raw) =>
            {
                Some((raw - 0x20, true))
            }
            MachineModel::X68000Xvi | MachineModel::X68030 if (0x20..=0x3f).contains(&raw) => {
                Some((raw - 0x20, true))
            }
            _ => None,
        }
    }

    fn is_disconnected_legacy_hdc(&self, address: u32) -> bool {
        self.model != MachineModel::X68000 && (HDC_BASE..HDC_BASE + 0x20).contains(&address)
    }
}

impl AddressBus for Bus {
    fn set_supervisor_mode(&mut self, supervisor: bool) {
        self.supervisor = supervisor;
    }

    fn read_byte(&mut self, address: u32) -> u8 {
        self.read_mapped(address).unwrap_or(0xff)
    }

    fn read_word(&mut self, address: u32) -> u16 {
        u16::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
        ])
    }

    fn read_long(&mut self, address: u32) -> u32 {
        u32::from_be_bytes([
            self.read_byte(address),
            self.read_byte(address.wrapping_add(1)),
            self.read_byte(address.wrapping_add(2)),
            self.read_byte(address.wrapping_add(3)),
        ])
    }

    fn write_byte(&mut self, address: u32, value: u8) {
        self.write_mapped(address, value);
    }

    fn write_word(&mut self, address: u32, value: u16) {
        let [a, b] = value.to_be_bytes();
        self.write_byte(address, a);
        self.write_byte(address.wrapping_add(1), b);
    }

    fn write_long(&mut self, address: u32, value: u32) {
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            self.write_byte(address.wrapping_add(offset as u32), byte);
        }
    }

    fn try_read_byte(&mut self, address: u32) -> Result<u8, BusFault> {
        match self.read_mapped(address) {
            Some(value) => Ok(value),
            None => Err(self.record_bus_fault(address)),
        }
    }

    fn try_read_word(&mut self, address: u32) -> Result<u16, BusFault> {
        Ok(u16::from_be_bytes([
            self.try_read_byte(address)?,
            self.try_read_byte(address.wrapping_add(1))?,
        ]))
    }

    fn try_read_long(&mut self, address: u32) -> Result<u32, BusFault> {
        Ok(u32::from_be_bytes([
            self.try_read_byte(address)?,
            self.try_read_byte(address.wrapping_add(1))?,
            self.try_read_byte(address.wrapping_add(2))?,
            self.try_read_byte(address.wrapping_add(3))?,
        ]))
    }

    fn try_write_byte(&mut self, address: u32, value: u8) -> Result<(), BusFault> {
        if self.write_mapped(address, value) {
            Ok(())
        } else {
            Err(self.record_bus_fault(address))
        }
    }

    fn try_write_word(&mut self, address: u32, value: u16) -> Result<(), BusFault> {
        let [high, low] = value.to_be_bytes();
        self.try_write_byte(address, high)?;
        self.try_write_byte(address.wrapping_add(1), low)
    }

    fn try_write_long(&mut self, address: u32, value: u32) -> Result<(), BusFault> {
        for (offset, byte) in value.to_be_bytes().into_iter().enumerate() {
            self.try_write_byte(address.wrapping_add(offset as u32), byte)?;
        }
        Ok(())
    }

    fn interrupt_acknowledge(&mut self, level: u8) -> u32 {
        if level == 6
            && let Some(vector) = self.devices.mfp.acknowledge()
        {
            return u32::from(vector);
        }
        if level == 1 {
            self.refresh_ioc_requests();
        }
        if level == 1 && self.devices.ioc_request & 0x04 != 0 {
            self.devices.ioc_request &= !0x04;
            self.devices.ioc_ack_count = self.devices.ioc_ack_count.wrapping_add(1);
            return u32::from(self.devices.ioc[3]);
        }
        if level == 1 && self.devices.ioc_request & 0x02 != 0 {
            self.devices.ioc_request &= !0x02;
            self.devices.ioc_ack_count = self.devices.ioc_ack_count.wrapping_add(1);
            self.devices.fdc.acknowledge_media();
            return u32::from(self.devices.ioc[3] | 1);
        }
        if level == 1 && self.devices.ioc_request & 0x08 != 0 {
            self.devices.ioc_request &= !0x08;
            self.devices.ioc_ack_count = self.devices.ioc_ack_count.wrapping_add(1);
            self.devices.hdc.acknowledge();
            return u32::from(self.devices.ioc[3] | 2);
        }
        if level == 3
            && let Some(vector) = self.devices.dma.acknowledge()
        {
            return u32::from(vector);
        }
        if level == 4 && self.devices.midi.interrupt_pending() {
            return u32::from(self.devices.midi.acknowledge());
        }
        if level == 5 && self.devices.scc.interrupt_pending() {
            return u32::from(self.devices.scc.acknowledge());
        }
        if level == self.devices.irq_level {
            self.devices.irq_level = 0;
        }
        if level == 1 {
            self.devices.ioc_spurious_ack_count =
                self.devices.ioc_spurious_ack_count.wrapping_add(1);
            0x18
        } else {
            u32::from(self.devices.irq_vector)
        }
    }

    fn reset_devices(&mut self) {
        self.reset();
    }
}

fn fault(address: u32) -> BusFault {
    BusFault {
        kind: BusFaultKind::BusError,
        address,
    }
}

fn get_range(bytes: &[u8], base: u32, address: u32) -> Option<u8> {
    address
        .checked_sub(base)
        .and_then(|offset| bytes.get(offset as usize).copied())
}

fn set_range(bytes: &mut [u8], base: u32, address: u32, value: u8) -> bool {
    let Some(offset) = address.checked_sub(base) else {
        return false;
    };
    let Some(byte) = bytes.get_mut(offset as usize) else {
        return false;
    };
    *byte = value;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use m68k::{CpuCore, CpuType, StepResult};

    fn write_word(bus: &mut Bus, address: u32, value: u16) {
        bus.ram[address as usize..address as usize + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_long(bus: &mut Bus, address: u32, value: u32) {
        bus.ram[address as usize..address as usize + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn exception_bus() -> Bus {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        write_long(&mut bus, 0, 0x0008_0000);
        write_long(&mut bus, 4, 0x0000_0200);
        write_long(&mut bus, 2 * 4, 0x0000_0400);
        write_long(&mut bus, 3 * 4, 0x0000_0420);
        // bus-error handler: moveq #2,d7 / bra.s *
        write_word(&mut bus, 0x400, 0x7e02);
        write_word(&mut bus, 0x402, 0x60fe);
        // address-error handler: moveq #3,d7 / bra.s *
        write_word(&mut bus, 0x420, 0x7e03);
        write_word(&mut bus, 0x422, 0x60fe);
        bus
    }

    #[test]
    fn model_masks_addresses() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.write_byte(0x0100_0020, 0x55);
        assert_eq!(bus.ram[0x20], 0x55);
        bus.model = MachineModel::X68030;
        assert!(bus.try_write_byte(0x0100_0020, 0x11).is_ok());
        assert_eq!(bus.try_read_byte(0x0100_0020).unwrap(), 0xff);
        bus.write_byte(0xff00_0020, 0x66);
        assert_eq!(bus.ram[0x20], 0x66);
    }

    #[test]
    fn fresh_sram_uses_initialized_reserved_bytes() {
        let bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert!(bus.sram.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn sram_ram_size_word_reports_installed_memory() {
        let mut bus = Bus::new(&MachineConfig::default(), 4 * 1024 * 1024);
        assert_eq!(bus.read_word(SRAM_BASE + 8), 0x0040);
        assert_eq!(bus.read_byte(SRAM_BASE + 7), 0);
        assert_eq!(bus.read_byte(SRAM_BASE + 10), 0);
    }

    #[test]
    fn missing_cgrom_is_mapped_as_blank_font_instead_of_bus_error() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert_eq!(bus.try_read_byte(CGROM_BASE).unwrap(), 0);
        assert_eq!(bus.try_read_byte(CGROM_BASE + CGROM_SIZE - 1).unwrap(), 0);
        // 直後の$FC0000は未装着内蔵SCSI ROMのopen-bus窓。
        assert_eq!(bus.try_read_byte(CGROM_BASE + CGROM_SIZE).unwrap(), 0xff);
        bus.cgrom = vec![0xa5; CGROM_SIZE as usize];
        assert_eq!(bus.try_read_byte(CGROM_BASE + 123).unwrap(), 0xa5);
    }

    #[test]
    fn mfp_gpip_exposes_horizontal_sync_edges() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert_ne!(bus.read_io(MFP_BASE + 1).unwrap() & 0x80, 0);

        let until_sync = bus.scheduler.cycles_until_next_event();
        bus.tick(until_sync);
        assert_eq!(
            bus.read_io(MFP_BASE + 1).unwrap() & 0x80,
            0,
            "HSync pulse must become active-low so polling software can observe it"
        );

        let until_line_end = bus.scheduler.cycles_until_next_event();
        bus.tick(until_line_end);
        assert_ne!(
            bus.read_io(MFP_BASE + 1).unwrap() & 0x80,
            0,
            "HSync must return high at the scanline boundary"
        );
    }

    #[test]
    fn vertical_display_interrupt_respects_the_mfp_active_edge() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.devices.mfp.write(0x07, 0); // IERA: HSync/raster/timerを無効化
        bus.devices.mfp.write(0x09, 0x40); // IERB: GPIP4 (source 9)
        bus.devices.mfp.write(0x15, 0x40); // IMRB

        // reset時のAER bit 4は0なので、可視領域開始のrising edgeではなく
        // 終了時のfalling edgeだけが割り込みになる。
        while !bus.devices.crtc.at_visible_start() {
            bus.tick(bus.scheduler.cycles_until_next_event());
        }
        assert!(!bus.devices.mfp.has_interrupt());
        for _ in 0..u32::from(bus.devices.crtc.v_total()) * 2 {
            if bus.devices.mfp.has_interrupt() {
                break;
            }
            bus.tick(bus.scheduler.cycles_until_next_event());
        }
        assert!(bus.devices.mfp.has_interrupt());

        // pendingを消してrising edgeへ切り替えると、次の可視領域開始で発生する。
        bus.devices.mfp.write(0x0d, 0); // IPRB clear
        bus.devices.mfp.write(0x03, 0x16); // AER GPIP4 rising
        while !bus.devices.crtc.at_visible_start() {
            bus.tick(bus.scheduler.cycles_until_next_event());
        }
        assert!(bus.devices.mfp.has_interrupt());
    }

    #[test]
    fn scanout_latches_each_line_before_raster_sprite_updates() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.devices.video.write(0x601, 0x40); // sprite/BG layer enable
        bus.devices.video.write(0x202, 0x11);
        bus.devices.video.write(0x203, 0x11);
        bus.sprite_ram.write(1, 16);
        bus.sprite_ram.write(3, 16);
        bus.sprite_ram.write(7, 3);
        bus.sprite_ram.write(0x8000, 0x10); // pattern 0, row 0
        bus.sprite_ram.write(0x8004, 0x10); // pattern 0, row 1
        bus.sprite_ram.write(0x808, 2);

        while bus.devices.crtc.visible_output_lines()[0] != Some(0) {
            let cycles = bus.scheduler.cycles_until_next_event();
            bus.tick(cycles);
        }
        bus.tick(bus.scheduler.cycles_until_next_event()); // visible末尾のHSYNCで0行目を確定
        assert_eq!(bus.scanout_frame[0], 0x1111);

        // raster処理がpaletteを書き換えても、走査済みの0行目は変化せず、
        // 次に開始する1行目からだけ新しい色が反映される。
        bus.devices.video.write(0x202, 0x22);
        bus.devices.video.write(0x203, 0x22);
        while bus.devices.crtc.visible_output_lines()[0] != Some(1) {
            let cycles = bus.scheduler.cycles_until_next_event();
            bus.tick(cycles);
        }
        bus.tick(bus.scheduler.cycles_until_next_event()); // 1行目を確定
        assert_eq!(bus.scanout_frame[0], 0x1111);
        assert_eq!(bus.scanout_frame[bus.scanout_width as usize], 0x2222);
    }

    #[test]
    fn printer_write_only_data_and_strobe_ports_are_mapped() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert!(bus.try_write_byte(PRINTER_BASE + 1, 0xa5).is_ok());
        assert!(bus.try_write_byte(PRINTER_BASE + 3, 0).is_ok());
        assert_eq!(bus.devices.printer, [0xa5, 0]);
        assert!(bus.try_read_byte(PRINTER_BASE + 3).is_err());
    }

    #[test]
    fn area_set_blocks_user_mode_below_configured_boundary() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert!(bus.write_io(AREA_BASE + 1, 2));
        bus.set_supervisor(false);
        assert!(bus.try_read_byte(0x5fff).is_err());
        assert!(bus.try_read_long(8).is_err());
        // CPUの例外entryが同じ命令内でsupervisor遷移を通知する。
        bus.set_supervisor(true);
        assert!(bus.try_read_long(8).is_ok());
        bus.set_supervisor(false);
        assert!(bus.try_read_byte(0x6000).is_ok());
        assert!(bus.write_io(AREA_BASE + 1, 0x0d));
        assert!(bus.try_read_byte(0x1_bfff).is_err());
        bus.set_supervisor(false);
        assert!(bus.try_read_byte(0x1_c000).is_ok());
        bus.set_supervisor(true);
        assert!(bus.try_read_byte(0x100).is_ok());
    }

    #[test]
    fn reset_vectors_come_from_ipl_tail() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.ipl = vec![0; 0x20_000];
        bus.ipl[0x10_000] = 0x12;
        assert_eq!(bus.read_byte(0), 0x12);
        bus.release_reset_overlay();
        assert_eq!(bus.read_byte(0), 0);
    }

    #[test]
    fn writes_to_rom_windows_are_acknowledged_and_ignored() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.ipl = vec![0xa5; 0x20_000];
        bus.cgrom = vec![0x5a; CGROM_SIZE as usize];
        assert!(bus.try_write_byte(0x00ff_fff6, 0).is_ok());
        assert!(bus.try_write_byte(CGROM_BASE + 7, 0).is_ok());
        assert_eq!(bus.ipl[0x1_fff6], 0xa5);
        assert_eq!(bus.cgrom[7], 0x5a);
    }

    #[test]
    fn xvi_and_x68030_have_a_blank_internal_scsi_rom_window_when_unloaded() {
        for model in [MachineModel::X68000Xvi, MachineModel::X68030] {
            let mut config = MachineConfig::default();
            config.model = model;
            let mut bus = Bus::new(&config, 1024 * 1024);
            assert_eq!(bus.try_read_byte(0x00fd_fff6).unwrap(), 0xff);
            assert!(bus.try_write_byte(0x00fd_fff6, 0).is_ok());
        }
    }

    #[test]
    fn eight_kib_scsi_rom_does_not_shadow_midi_on_x68030() {
        let mut x68030 = MachineConfig::default();
        x68030.model = MachineModel::X68030;
        let mut bus = Bus::new(&x68030, 1024 * 1024);
        bus.scsi_rom = vec![0x5a; 0x2000];
        assert_eq!(bus.try_read_byte(0x00fc_0000).unwrap(), 0x5a);
        // $EAFA00 is the MIDI window; an internal SCSI ROM must not shadow
        // it merely because both images are 8 KiB.
        assert_eq!(bus.try_read_byte(MIDI_BASE).unwrap(), 0);

        let mut xvi = MachineConfig::default();
        xvi.model = MachineModel::X68000Xvi;
        let mut bus = Bus::new(&xvi, 1024 * 1024);
        bus.scsi_rom = vec![0x5a; 0x2000];
        assert_eq!(bus.try_read_byte(0x00ea_0000).unwrap(), 0x5a);
        assert_eq!(bus.try_read_byte(0x00fd_fff6).unwrap(), 0xff);
    }

    #[test]
    fn odd_word_access_takes_68000_address_error() {
        let mut bus = exception_bus();
        // move.w $000001.l,d0
        write_word(&mut bus, 0x200, 0x3039);
        write_long(&mut bus, 0x202, 1);
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.reset(&mut bus);
        cpu.execute(&mut bus, 100);
        assert_eq!(cpu.d(7), 3);
        assert_eq!(cpu.sp(), 0x0008_0000 - 14);
    }

    #[test]
    fn unmapped_access_takes_68000_bus_error() {
        let mut bus = exception_bus();
        // move.b $00a00000.l,d0
        write_word(&mut bus, 0x200, 0x1039);
        write_long(&mut bus, 0x202, 0x00a0_0000);
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.reset(&mut bus);
        cpu.execute(&mut bus, 100);
        assert_eq!(cpu.d(7), 2);
        assert_eq!(cpu.sp(), 0x0008_0000 - 14);
    }

    #[test]
    fn interrupt_mask_and_device_vector_are_honoured() {
        let mut bus = exception_bus();
        write_long(&mut bus, 0x40 * 4, 0x0000_0440);
        write_word(&mut bus, 0x200, 0x4e71); // nop
        write_word(&mut bus, 0x202, 0x60fe);
        write_word(&mut bus, 0x440, 0x7e04); // moveq #4,d7
        write_word(&mut bus, 0x442, 0x60fe);
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.reset(&mut bus);
        cpu.set_sr_noint_nosp(0x2500);
        bus.devices.irq_level = 4;
        bus.devices.irq_vector = 0x40;
        cpu.set_irq(bus.pending_irq());
        cpu.step(&mut bus);
        assert_eq!(cpu.d(7), 0, "level 4 must be masked by SR level 5");
        bus.devices.irq_level = 6;
        cpu.set_irq(bus.pending_irq());
        cpu.step(&mut bus);
        cpu.step(&mut bus);
        assert_eq!(cpu.d(7), 4);
        assert_eq!(bus.pending_irq(), 0);
    }

    #[test]
    fn accepted_interrupt_wakes_single_step_cpu_from_stop() {
        let mut bus = exception_bus();
        write_long(&mut bus, 0x40 * 4, 0x0000_0440);
        write_word(&mut bus, 0x200, 0x4e72); // stop #$2000
        write_word(&mut bus, 0x202, 0x2000);
        write_word(&mut bus, 0x440, 0x7e04); // moveq #4,d7
        let mut cpu = CpuCore::new();
        cpu.set_cpu_type(CpuType::M68000);
        cpu.reset(&mut bus);
        assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
        assert_ne!(cpu.stopped, 0);

        bus.devices.irq_level = 6;
        bus.devices.irq_vector = 0x40;
        cpu.set_irq(bus.pending_irq());
        assert!(matches!(cpu.step(&mut bus), StepResult::Ok { .. }));
        assert_eq!(cpu.stopped, 0);
        assert_eq!(cpu.pc, 0x440);
        cpu.step(&mut bus);
        assert_eq!(cpu.d(7), 4);
    }

    #[test]
    fn dma_moves_bytes_through_the_machine_bus() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.ram[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
        bus.devices.dma.write(0x06, 0x05); // MAR/DAR increment
        bus.devices.dma.write(0x0a, 0);
        bus.devices.dma.write(0x0b, 4);
        bus.devices.dma.write(0x0d, 0);
        bus.devices.dma.write(0x0e, 1);
        bus.devices.dma.write(0x0f, 0);
        bus.devices.dma.write(0x15, 0);
        bus.devices.dma.write(0x16, 2);
        bus.devices.dma.write(0x17, 0);
        bus.devices.dma.write(0x07, 0x80);
        bus.tick(32);
        assert_eq!(&bus.ram[0x200..0x204], &[1, 2, 3, 4]);
        assert_eq!(bus.devices.dma.read(0), 0x80);
    }

    #[test]
    fn dma_array_chain_loads_descriptors_from_guest_ram() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        bus.ram[0x200] = 0x31;
        bus.ram[0x201] = 0x42;
        write_long(&mut bus, 0x100, 0x200);
        write_word(&mut bus, 0x104, 1);
        write_long(&mut bus, 0x106, 0x201);
        write_word(&mut bus, 0x10a, 1);

        bus.devices.dma.write(0x05, 0x08); // array chain
        bus.devices.dma.write(0x06, 0x05); // MAR/DAR increment
        bus.devices.dma.write(0x16, 0x03);
        bus.devices.dma.write(0x17, 0x00);
        bus.devices.dma.write(0x1b, 2);
        bus.devices.dma.write(0x1e, 0x01);
        bus.devices.dma.write(0x1f, 0x00);
        bus.devices.dma.write(0x07, 0x80);
        bus.service_dma_chain(0);
        bus.tick(32);

        assert_eq!(&bus.ram[0x300..0x302], &[0x31, 0x42]);
        assert_eq!(bus.devices.dma.read(0) & 0x80, 0x80);
    }

    #[test]
    fn fdc_data_phase_transfers_through_hd63450_dma() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        let mut disk = vec![0; 77 * 2 * 8 * 1024];
        for (index, byte) in disk[..1024].iter_mut().enumerate() {
            *byte = (index ^ (index >> 3)) as u8;
        }
        bus.media.insert(
            DriveId::Floppy(0),
            MediaImage::parse(crate::MediaFormat::Xdf, &disk, false).unwrap(),
        );
        bus.devices.dma.write(0x05, 0x80); // device -> memory, byte
        bus.devices.dma.write(0x06, 0x04); // MAR increment, DAR fixed
        bus.devices.dma.write(0x0a, 0x04);
        bus.devices.dma.write(0x0b, 0x00);
        for (offset, value) in [0x00, 0x00, 0x10, 0x00].into_iter().enumerate() {
            bus.devices.dma.write(0x0c + offset as u32, value);
        }
        for (offset, value) in FDC_BASE.to_be_bytes().into_iter().enumerate() {
            let value = if offset == 3 { 3 } else { value };
            bus.devices.dma.write(0x14 + offset as u32, value);
        }
        bus.devices.dma.write(0x07, 0x80);
        bus.tick(4096);
        assert_eq!(bus.devices.dma.read(0x0a), 0x04);
        assert_eq!(bus.devices.dma.read(0x0b), 0x00);
        assert_eq!(bus.devices.dma.read(0) & 0x08, 0x08);

        // IPLと同じくDMACを先にstartし、FDC commandを後から送る。
        for byte in [0x06, 0, 0, 0, 1, 3, 1, 0x1b, 0xff] {
            assert!(bus.write_io(FDC_BASE + 3, byte));
        }
        bus.tick(4096);

        assert_eq!(&bus.ram[0x1000..0x1400], &disk[..1024]);
        assert_eq!(bus.devices.dma.read(0) & 0x80, 0x80);
    }

    #[test]
    fn ioc_reports_and_vectors_fdc_and_hdd_interrupts() {
        let mut bus = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert!(bus.write_io(IOC_BASE + 1, 0x04));
        assert!(bus.write_io(IOC_BASE + 3, 0x44));
        // recalibrate drive 0 -> FDC IRQ
        assert!(bus.write_io(FDC_BASE + 3, 0x07));
        assert!(bus.write_io(FDC_BASE + 3, 0x00));
        assert_eq!(bus.pending_irq(), 1);
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x80, 0x80);
        assert_eq!(bus.interrupt_acknowledge(1), 0x44);
        assert_eq!(
            bus.pending_irq(),
            0,
            "IOC acknowledge clears the latched request while FDC INT stays high"
        );
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x80, 0x80);
        // uPD72065のseek/recalibrate IRQはCPU acknowledgeではなく、
        // Sense Interrupt StatusでST0/cylinderを取り出した時に解除される。
        assert!(bus.write_io(FDC_BASE + 3, 0x08));
        assert_eq!(bus.read_io(FDC_BASE + 3), Some(0x20));
        assert_eq!(bus.read_io(FDC_BASE + 3), Some(0));
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x80, 0);

        // 信号が一度lowになった後の次の立ち上がりは再びrequestされる。
        assert!(bus.write_io(FDC_BASE + 3, 0x07));
        assert!(bus.write_io(FDC_BASE + 3, 0x00));
        assert_eq!(bus.pending_irq(), 1);
        assert_eq!(bus.interrupt_acknowledge(1), 0x44);
        assert!(bus.write_io(FDC_BASE + 3, 0x08));
        assert_eq!(bus.read_io(FDC_BASE + 3), Some(0x20));
        assert_eq!(bus.read_io(FDC_BASE + 3), Some(0));

        assert!(bus.write_io(IOC_BASE + 1, 0x02));
        bus.notify_media_change(DriveId::Floppy(3));
        assert_eq!(bus.pending_irq(), 1);
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x40, 0x40);
        assert_eq!(bus.interrupt_acknowledge(1), 0x45);
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x40, 0);

        assert!(bus.write_io(IOC_BASE + 1, 0x08));
        // target 0を選択し、媒体なしでTEST UNIT READYを発行 -> HDD IRQ
        assert!(bus.write_io(HDC_BASE + 7, 0x01));
        assert!(bus.write_io(HDC_BASE + 3, 0));
        for byte in [0, 0, 0, 0, 0, 0] {
            assert!(bus.write_io(HDC_BASE + 1, byte));
        }
        assert_eq!(bus.pending_irq(), 1);
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x10, 0x10);
        assert_eq!(bus.interrupt_acknowledge(1), 0x46);
        assert_eq!(bus.read_io(IOC_BASE + 1).unwrap() & 0x10, 0);
    }

    #[test]
    fn hdc_register_window_matches_sasi_and_internal_scsi_models() {
        let mut original = Bus::new(&MachineConfig::default(), 1024 * 1024);
        assert!(original.read_io(HDC_BASE + 1).is_some());
        assert!(original.read_io(HDC_BASE + 0x21).is_none());
        original.scsi_rom = vec![0; 0x2000];
        assert!(original.read_io(HDC_BASE + 0x21).is_some());

        for model in [MachineModel::X68000Xvi, MachineModel::X68030] {
            let mut bus = Bus::new(
                &MachineConfig {
                    model,
                    ..MachineConfig::default()
                },
                1024 * 1024,
            );
            assert_eq!(bus.read_io(HDC_BASE + 1), Some(0xff));
            assert!(bus.write_io(HDC_BASE + 1, 0x55));
            assert!(bus.read_io(HDC_BASE + 0x21).is_some());
        }
    }
}
