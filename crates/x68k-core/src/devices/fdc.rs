//! uPD72065 互換 FDC のコマンド、実行、結果フェーズ。
//!
//! コマンド長と状態ビットは PX68k `x68k/fdc.c` を比較資料としている。

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use crate::DriveId;
use crate::media::MediaImage;

const PARAMETER_COUNTS: [u8; 32] = [
    0, 0, 8, 2, 1, 8, 8, 1, 0, 8, 1, 0, 8, 5, 0, 2, 0, 8, 0, 0, 0, 0, 0, 0, 0, 8, 0, 0, 0, 8, 0, 0,
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Fdc {
    command: u8,
    multi_track: bool,
    skip_deleted: bool,
    parameters: Vec<u8>,
    expected_parameters: u8,
    output: VecDeque<u8>,
    /// `output`末尾に積まれた結果フェーズの残りbyte数。
    ///
    /// 読み出しデータと結果を同じFIFOへ保持しているため、これを別に追跡して
    /// 実行フェーズの最終byteが転送された時点でだけIRQを立ち上げる。
    #[serde(default)]
    result_bytes_remaining: u8,
    #[serde(default)]
    last_result_status: [u8; 3],
    write_data: Vec<u8>,
    expected_write_data: usize,
    selected_drive: Option<u8>,
    cylinders: [u8; 4],
    control: u8,
    irq_pending: bool,
    media_irq_pending: bool,
    command_count: u64,
    sector_read_count: u64,
}

impl Default for Fdc {
    /// ハードウェアのリセット直後に相当する既定状態を構築して返す。
    fn default() -> Self {
        Self {
            command: 0,
            multi_track: false,
            skip_deleted: false,
            parameters: Vec::new(),
            expected_parameters: 0,
            output: VecDeque::new(),
            result_bytes_remaining: 0,
            last_result_status: [0; 3],
            write_data: Vec::new(),
            expected_write_data: 0,
            selected_drive: None,
            cylinders: [0; 4],
            control: 0,
            irq_pending: false,
            media_irq_pending: false,
            command_count: 0,
            sector_read_count: 0,
        }
    }
}

impl Fdc {
    /// 対象のメモリまたはレジスタを読み取り、規定の読出し副作用を反映して値を返す。
    pub(crate) fn read(&mut self, offset: u32, media: &BTreeMap<DriveId, MediaImage>) -> u8 {
        match offset {
            1 => self.main_status(),
            3 => self.read_data_register(),
            5 => {
                let inserted = (0..4).any(|drive| {
                    self.control & (1 << drive) != 0 && media.contains_key(&DriveId::Floppy(drive))
                });
                if inserted { 0x80 } else { 0 }
            }
            _ => 0,
        }
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、関連する副作用を反映する。
    pub(crate) fn write(
        &mut self,
        offset: u32,
        value: u8,
        media: &mut BTreeMap<DriveId, MediaImage>,
    ) {
        match offset {
            3 => self.write_data_register(value, media),
            5 => self.control = value,
            // bit 7はmotor、bit 4は転送rate。選択ドライブ自体は常にbits 1-0。
            7 => self.selected_drive = Some(value & 3),
            _ => {}
        }
    }

    /// `interrupt_pending` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn interrupt_pending(&self) -> bool {
        self.irq_pending
    }

    /// `media_interrupt_pending` の条件が現在成立しているかを、副作用なく判定して返す。
    pub(crate) fn media_interrupt_pending(&self) -> bool {
        self.media_irq_pending
    }

    /// FDDの挿入・排出変化をIOC通知用の割り込み状態へ反映する。
    pub(crate) fn notify_media_change(&mut self, drive: u8) {
        if drive < 4 {
            self.selected_drive = Some(drive);
            self.media_irq_pending = true;
        }
    }

    /// 割り込み状態を更新し、CPUと周辺機器のハンドシェイクを進める。
    pub(crate) fn acknowledge_media(&mut self) {
        self.media_irq_pending = false;
    }

    /// 現在の状態や結果を利用者向けの診断情報として提示する。
    pub(crate) fn diagnostics(&self) -> (u64, u64, u8, u8, usize) {
        (
            self.command_count,
            self.sector_read_count,
            self.command,
            self.main_status(),
            self.output.len(),
        )
    }

    /// 現在キューにある結果フェーズのST0/ST1/ST2。読み出しは消費しない。
    pub(crate) fn result_status(&self) -> [u8; 3] {
        self.last_result_status
    }

    /// 受信中のFDCコマンド引数を診断用の固定長配列で返す。
    pub(crate) fn command_parameters(&self) -> [u8; 8] {
        std::array::from_fn(|index| self.parameters.get(index).copied().unwrap_or(0))
    }

    /// DMAのTerminal Count入力。read execution phaseを打ち切り、未転送の
    /// sector dataを捨てて、既にFIFO末尾へ用意した結果フェーズへ進める。
    pub(crate) fn terminal_count(&mut self) {
        if self.result_bytes_remaining == 0 {
            return;
        }
        while self.output.len() > usize::from(self.result_bytes_remaining) {
            self.output.pop_front();
        }
        if self.output.len() == usize::from(self.result_bytes_remaining) {
            self.irq_pending = true;
        }
    }

    /// FDC -> memory DMA request.  Result bytes are consumed by the CPU and
    /// must not keep DREQ asserted after the execution data has ended.
    pub(crate) fn dma_read_ready(&self) -> bool {
        self.output.len() > usize::from(self.result_bytes_remaining)
    }

    /// Memory -> FDC DMA request during a write/format execution phase.
    pub(crate) fn dma_write_ready(&self) -> bool {
        self.expected_write_data != 0 && self.write_data.len() < self.expected_write_data
    }

    /// 現在の状態または入力から `main_status` に対応する値を算出し、副作用なく返す。
    fn main_status(&self) -> u8 {
        let busy = self.expected_parameters != 0
            || self.expected_write_data != 0
            || !self.output.is_empty();
        0x80 | if !self.output.is_empty() { 0x40 } else { 0 } | if busy { 0x10 } else { 0 }
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_data_register(&mut self) -> u8 {
        let reading_result = self.result_bytes_remaining != 0
            && self.output.len() <= usize::from(self.result_bytes_remaining);
        let value = self.output.pop_front().unwrap_or(0);
        if reading_result {
            self.result_bytes_remaining -= 1;
            if self.result_bytes_remaining == 0 {
                // uPD72065のINT出力は結果フェーズの最終byteを読み終えると下がる。
                self.irq_pending = false;
            }
        } else if self.result_bytes_remaining != 0
            && self.output.len() == usize::from(self.result_bytes_remaining)
        {
            // DMA/PIOのデータ転送完了後、結果フェーズへ入った瞬間にINTを上げる。
            self.irq_pending = true;
        }
        value
    }

    /// 対象のメモリまたはレジスタへ値を書き込み、必要な副作用を反映する。
    fn write_data_register(&mut self, value: u8, media: &mut BTreeMap<DriveId, MediaImage>) {
        if self.expected_write_data != 0 {
            self.write_data.push(value);
            if self.write_data.len() == self.expected_write_data {
                self.finish_sector_write(media);
            }
            return;
        }
        if self.expected_parameters != 0 {
            self.parameters.push(value);
            self.expected_parameters -= 1;
            if self.expected_parameters == 0 {
                self.execute(media);
            }
            return;
        }
        self.command = value & 0x1f;
        self.command_count = self.command_count.wrapping_add(1);
        self.multi_track = value & 0x80 != 0;
        self.skip_deleted = value & 0x20 != 0;
        self.parameters.clear();
        self.output.clear();
        self.result_bytes_remaining = 0;
        // Sense Interrupt Statusは直前のseek/recalibrate完了INTを調べるため、
        // command byteを書いた時点ではその信号を保持する。
        if self.command != 8 {
            self.irq_pending = false;
        }
        self.expected_parameters = PARAMETER_COUNTS[self.command as usize];
        if self.expected_parameters == 0 {
            self.execute(media);
        }
    }

    /// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
    fn execute(&mut self, media: &mut BTreeMap<DriveId, MediaImage>) {
        match self.command {
            3 => {}
            4 => {
                let drive = self.parameter(0) & 3;
                let mut status = drive;
                if self.cylinders[drive as usize] == 0 {
                    status |= 0x10;
                }
                if let Some(image) = media.get(&DriveId::Floppy(drive)) {
                    status |= 0x20;
                    if image.write_protected {
                        status |= 0x40;
                    }
                }
                self.output.push_back(status);
            }
            5 | 9 => {
                let size = self.sector_size();
                if self.current_image(media).is_none() {
                    self.queue_result(0x48, 0x04, 0);
                } else if self
                    .current_image(media)
                    .is_some_and(|image| image.write_protected)
                {
                    self.queue_result(0x40, 0x02, 0);
                } else {
                    self.expected_write_data = size * self.transfer_sector_count();
                    self.write_data.clear();
                }
            }
            2 | 6 | 12 => self.read_sector(media),
            13 => {
                let sectors = usize::from(self.parameter(2).max(1));
                self.expected_write_data = sectors * 4;
                self.write_data.clear();
            }
            7 => {
                let drive = self.parameter(0) & 3;
                self.cylinders[drive as usize] = 0;
                self.irq_pending = true;
            }
            8 => {
                if self.irq_pending {
                    let drive = self.selected_drive.unwrap_or(0).min(3);
                    self.output
                        .extend([0x20 | drive, self.cylinders[drive as usize]]);
                    self.irq_pending = false;
                } else {
                    // seek/recalibrate完了がqueueされていなければInvalid Command。
                    // 毎回架空の完了を返すとIPLの割り込み排出ループが終わらない。
                    self.output.push_back(0x80);
                }
            }
            10 => {
                let drive = self.parameter(0) & 3;
                let cylinder = self.cylinders[drive as usize];
                let ready = media.contains_key(&DriveId::Floppy(drive));
                self.queue_result(
                    if ready { 0x20 | drive } else { 0x48 | drive },
                    if ready { 0 } else { 4 },
                    0,
                );
                if ready {
                    let length = self.output.len();
                    self.output[length - 4] = cylinder;
                    self.output[length - 3] = 0;
                    self.output[length - 2] = 1;
                    self.output[length - 1] = 3;
                }
            }
            15 => {
                let drive = self.parameter(0) & 3;
                self.cylinders[drive as usize] = self.parameter(1);
                self.irq_pending = true;
            }
            17 | 25 | 29 => {
                self.expected_write_data = self.sector_size() * self.transfer_sector_count();
                self.write_data.clear();
            }
            _ => self.output.push_back(0x80),
        }
    }

    /// 対象のメモリまたはレジスタを読み取り、現在値を呼び出し側へ返す。
    fn read_sector(&mut self, media: &BTreeMap<DriveId, MediaImage>) {
        let drive = self.parameter(0) & 3;
        let mut last = self.sector_id();
        let mut status1 = 0;
        let mut status2 = 0;
        let mut transferred = false;
        for sector in self.transfer_sector_ids() {
            let Some(image) = media.get(&DriveId::Floppy(drive)) else {
                self.queue_result(0x40 | drive, 0x04, 0);
                return;
            };
            let Some(bytes) = image.read_sector(sector[0], sector[1], sector[2], sector[3]) else {
                self.queue_result(0x40 | drive, 0x04, 0);
                return;
            };
            let deleted = image
                .sector_deleted(sector[0], sector[1], sector[2], sector[3])
                .unwrap_or(false);
            match image
                .sector_status(sector[0], sector[1], sector[2], sector[3])
                .unwrap_or(0)
            {
                0xa0 => status1 |= 0x20,
                0xb0 => {
                    status1 |= 0x20;
                    status2 |= 0x20;
                }
                0xf0 => status2 |= 0x01,
                _ => {}
            }
            let expected_deleted = self.command == 12;
            if self.command != 2 && deleted != expected_deleted {
                last = sector;
                if self.skip_deleted {
                    continue;
                }
                status2 |= 0x40;
            }
            self.output.extend(bytes);
            self.sector_read_count = self.sector_read_count.wrapping_add(1);
            last = sector;
            transferred = true;
        }
        self.queue_result_for_id(
            if transferred && status1 == 0 && status2 & 0x21 == 0 {
                drive
            } else {
                0x40 | drive
            },
            if transferred { status1 } else { 0x04 },
            status2,
            last,
        );
    }

    /// FDC書込みデータを媒体オーバーレイへ反映して結果フェーズへ進める。
    fn finish_sector_write(&mut self, media: &mut BTreeMap<DriveId, MediaImage>) {
        let drive = self.parameter(0) & 3;
        if self.command == 13 {
            self.finish_format(media);
            return;
        }
        if matches!(self.command, 17 | 25 | 29) {
            self.finish_scan(media);
            return;
        }
        let size = self.sector_size();
        let mut success = true;
        let mut last = self.sector_id();
        for (index, sector) in self.transfer_sector_ids().into_iter().enumerate() {
            let range = index * size..(index + 1) * size;
            success &= self
                .write_data
                .get(range)
                .and_then(|bytes| {
                    media.get_mut(&DriveId::Floppy(drive)).map(|image| {
                        image.write_sector_deleted(
                            sector[0],
                            sector[1],
                            sector[2],
                            sector[3],
                            bytes,
                            self.command == 9,
                        )
                    })
                })
                .unwrap_or(false);
            last = sector;
        }
        self.expected_write_data = 0;
        self.queue_result_for_id(
            if success { drive } else { 0x40 | drive },
            if success { 0 } else { 4 },
            0,
            last,
        );
    }

    /// FDC FORMAT TRACKの受信データを媒体オーバーレイへ反映して結果を返す。
    fn finish_format(&mut self, media: &mut BTreeMap<DriveId, MediaImage>) {
        let drive = self.parameter(0) & 3;
        let fill = self.parameter(4);
        let mut success = true;
        let mut last = [0; 4];
        if let Some(image) = media.get_mut(&DriveId::Floppy(drive)) {
            for id in self.write_data.chunks_exact(4) {
                last.copy_from_slice(id);
                let size = 128usize.checked_shl(u32::from(id[3])).unwrap_or(0);
                success &=
                    size != 0 && image.write_sector(id[0], id[1], id[2], id[3], &vec![fill; size]);
            }
        } else {
            success = false;
        }
        self.expected_write_data = 0;
        self.queue_result_for_id(
            if success { drive } else { 0x40 | drive },
            if success { 0 } else { 4 },
            0,
            last,
        );
    }

    /// FDC SCANの比較結果をST2へ反映し、結果フェーズへ進める。
    fn finish_scan(&mut self, media: &BTreeMap<DriveId, MediaImage>) {
        let drive = self.parameter(0) & 3;
        let size = self.sector_size();
        let ids = self.transfer_sector_ids();
        let mut last = self.sector_id();
        let hit = ids.iter().enumerate().all(|(index, sector)| {
            last = *sector;
            let Some(disk) = media
                .get(&DriveId::Floppy(drive))
                .and_then(|image| image.read_sector(sector[0], sector[1], sector[2], sector[3]))
            else {
                return false;
            };
            let Some(host) = self.write_data.get(index * size..(index + 1) * size) else {
                return false;
            };
            scan_matches(self.command, &disk, host)
        });
        self.expected_write_data = 0;
        self.queue_result_for_id(
            if hit { drive } else { 0x40 | drive },
            0,
            if hit { 0x08 } else { 0x04 },
            last,
        );
    }

    /// 入力を処理待ちキューへ追加し、後続処理で利用できるようにする。
    fn queue_result(&mut self, status0: u8, status1: u8, status2: u8) {
        let id = self.sector_id();
        self.queue_result_for_id(status0, status1, status2, id);
    }

    /// 入力を処理待ちキューへ追加し、後続処理で利用できるようにする。
    fn queue_result_for_id(&mut self, status0: u8, status1: u8, status2: u8, id: [u8; 4]) {
        self.last_result_status = [status0, status1, status2];
        self.output
            .extend([status0, status1, status2, id[0], id[1], id[2], id[3]]);
        self.result_bytes_remaining = 7;
        // 読み出しコマンドではFIFO先頭にデータがある。結果だけのコマンドは
        // この時点で結果フェーズなので直ちにINTを上げる。
        self.irq_pending = self.output.len() == usize::from(self.result_bytes_remaining);
    }

    /// 現在の状態または入力から `parameter` に対応する値を算出し、副作用なく返す。
    fn parameter(&self, index: usize) -> u8 {
        self.parameters.get(index).copied().unwrap_or(0)
    }

    /// 現在の状態または入力から `sector_id` に対応する値を算出し、副作用なく返す。
    fn sector_id(&self) -> [u8; 4] {
        [
            self.parameter(1),
            self.parameter(2),
            self.parameter(3),
            self.parameter(4),
        ]
    }

    /// 現在の状態または入力から `sector_size` に対応する値を算出し、副作用なく返す。
    fn sector_size(&self) -> usize {
        if self.parameter(4) == 0 {
            usize::from(self.parameter(7).max(1))
        } else {
            128usize
                .checked_shl(u32::from(self.parameter(4)))
                .unwrap_or(0)
        }
    }

    /// 現在の状態または入力から `transfer_sector_count` に対応する値を算出し、副作用なく返す。
    fn transfer_sector_count(&self) -> usize {
        self.transfer_sector_ids().len()
    }

    /// FDCのマルチトラック条件から今回転送するCHRN列を組み立てる。
    fn transfer_sector_ids(&self) -> Vec<[u8; 4]> {
        let cylinder = self.parameter(1);
        let start_head = self.parameter(2);
        let start_sector = self.parameter(3);
        let size = self.parameter(4);
        let end_sector = self.parameter(5).max(start_sector);
        let mut ids = (start_sector..=end_sector)
            .map(|sector| [cylinder, start_head, sector, size])
            .collect::<Vec<_>>();
        if self.multi_track && start_head == 0 {
            ids.extend((1..=end_sector).map(|sector| [cylinder, 1, sector, size]));
        }
        ids
    }

    /// 選択中FDDに装着された媒体を取得し、未装着ならFDCエラーを返す。
    fn current_image<'a>(
        &self,
        media: &'a BTreeMap<DriveId, MediaImage>,
    ) -> Option<&'a MediaImage> {
        media.get(&DriveId::Floppy(self.parameter(0) & 3))
    }
}

/// FDC SCAN条件に従いホスト値とセクタ値の大小・ワイルドカードを比較する。
fn scan_matches(command: u8, disk: &[u8], host: &[u8]) -> bool {
    let ordering = disk
        .iter()
        .zip(host)
        .filter(|(_, expected)| **expected != 0xff)
        .find_map(|(actual, expected)| (actual != expected).then(|| actual.cmp(expected)));
    match command {
        17 => ordering.is_none(),
        25 => ordering.is_none_or(|value| value.is_le()),
        29 => ordering.is_none_or(|value| value.is_ge()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MediaFormat;

    /// FDCコマンドバイト列をデータレジスタへ順に送って実行させる。
    fn command(fdc: &mut Fdc, media: &mut BTreeMap<DriveId, MediaImage>, bytes: &[u8]) {
        for &byte in bytes {
            fdc.write(3, byte, media);
        }
    }

    #[test]
    /// `sense_interrupt_only_reports_a_real_seek_or_recalibrate_completion` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn sense_interrupt_only_reports_a_real_seek_or_recalibrate_completion() {
        let mut fdc = Fdc::default();
        let mut media = BTreeMap::new();
        command(&mut fdc, &mut media, &[0x08]);
        assert_eq!(fdc.read(3, &media), 0x80);

        command(&mut fdc, &mut media, &[0x0f, 0, 7]);
        assert!(fdc.interrupt_pending());
        command(&mut fdc, &mut media, &[0x08]);
        assert_eq!(fdc.read(3, &media), 0x20);
        assert_eq!(fdc.read(3, &media), 7);
        assert!(!fdc.interrupt_pending());
        command(&mut fdc, &mut media, &[0x08]);
        assert_eq!(fdc.read(3, &media), 0x80);
    }

    #[test]
    /// `reads_and_writes_xdf_sector_through_command_phases` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn reads_and_writes_xdf_sector_through_command_phases() {
        let mut bytes = vec![0; 77 * 2 * 8 * 1024];
        bytes[1024] = 0x5a;
        let mut media = BTreeMap::from([(
            DriveId::Floppy(0),
            MediaImage::parse(MediaFormat::Xdf, &bytes, false).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        command(&mut fdc, &mut media, &[0x06, 0, 0, 0, 2, 3, 2, 0x1b, 0xff]);
        assert_eq!(fdc.read(1, &media) & 0xd0, 0xd0);
        assert_eq!(fdc.read(3, &media), 0x5a);

        command(&mut fdc, &mut media, &[0x05, 0, 0, 0, 1, 3, 1, 0x1b, 0xff]);
        for _ in 0..1024 {
            fdc.write(3, 0xa5, &mut media);
        }
        assert_eq!(media[&DriveId::Floppy(0)].read(0), Some(0xa5));
    }

    #[test]
    /// `read_irq_is_asserted_only_for_the_result_phase` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn read_irq_is_asserted_only_for_the_result_phase() {
        let bytes = vec![0; 77 * 2 * 8 * 1024];
        let mut media = BTreeMap::from([(
            DriveId::Floppy(0),
            MediaImage::parse(MediaFormat::Xdf, &bytes, true).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        command(&mut fdc, &mut media, &[0x06, 0, 0, 0, 1, 3, 1, 0x1b, 0xff]);

        assert!(!fdc.interrupt_pending(), "data phase must not raise INT");
        for _ in 0..1023 {
            fdc.read(3, &media);
            assert!(!fdc.interrupt_pending());
        }
        fdc.read(3, &media);
        assert!(fdc.interrupt_pending(), "result phase must raise INT");
        for _ in 0..6 {
            fdc.read(3, &media);
            assert!(fdc.interrupt_pending());
        }
        fdc.read(3, &media);
        assert!(!fdc.interrupt_pending(), "last result byte must lower INT");
    }

    #[test]
    /// `dma_terminal_count_discards_untransferred_sector_tail` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn dma_terminal_count_discards_untransferred_sector_tail() {
        let bytes = vec![0; 77 * 2 * 8 * 1024];
        let mut media = BTreeMap::from([(
            DriveId::Floppy(0),
            MediaImage::parse(MediaFormat::Xdf, &bytes, true).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        command(&mut fdc, &mut media, &[0x06, 0, 0, 0, 1, 3, 1, 0x1b, 0xff]);
        for _ in 0..384 {
            fdc.read(3, &media);
        }
        assert_eq!(fdc.output.len(), 640 + 7);
        assert!(!fdc.interrupt_pending());

        fdc.terminal_count();
        assert_eq!(fdc.output.len(), 7);
        assert!(fdc.interrupt_pending());
    }

    #[test]
    /// `drive_control_reports_all_four_drives_and_selection_without_motor` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn drive_control_reports_all_four_drives_and_selection_without_motor() {
        let mut media = BTreeMap::from([(
            DriveId::Floppy(3),
            MediaImage::parse(MediaFormat::Xdf, &vec![0; 77 * 2 * 8 * 1024], true).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        fdc.write(5, 0b1000, &mut media);
        assert_eq!(fdc.read(5, &media), 0x80);
        fdc.write(7, 3, &mut media);
        assert_eq!(fdc.selected_drive, Some(3));
        fdc.notify_media_change(3);
        assert!(fdc.media_interrupt_pending());
        fdc.acknowledge_media();
        assert!(!fdc.media_interrupt_pending());
    }

    #[test]
    /// `multi_track_read_continues_from_head_zero_to_head_one` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn multi_track_read_continues_from_head_zero_to_head_one() {
        let mut bytes = vec![0; 77 * 2 * 8 * 1024];
        bytes[8 * 1024] = 0x7b;
        let mut media = BTreeMap::from([(
            DriveId::Floppy(0),
            MediaImage::parse(MediaFormat::Xdf, &bytes, false).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        command(&mut fdc, &mut media, &[0x86, 0, 0, 0, 1, 3, 1, 0x1b, 0xff]);
        for _ in 0..1024 {
            fdc.read(3, &media);
        }
        assert_eq!(fdc.read(3, &media), 0x7b);
        for _ in 1..1024 {
            fdc.read(3, &media);
        }
        assert_eq!(fdc.read(3, &media) & 0xc0, 0);
    }

    #[test]
    /// `scan_comparison_honours_ff_wildcards_and_order` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn scan_comparison_honours_ff_wildcards_and_order() {
        assert!(scan_matches(17, &[1, 2, 3], &[1, 0xff, 3]));
        assert!(scan_matches(25, &[1, 2], &[2, 0]));
        assert!(!scan_matches(25, &[3, 0], &[2, 0]));
        assert!(scan_matches(29, &[3, 0], &[2, 0]));
        assert!(!scan_matches(29, &[1, 2], &[2, 0]));
    }

    #[test]
    /// `d88_crc_status_reaches_fdc_result_phase` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn d88_crc_status_reaches_fdc_result_phase() {
        const HEADER: usize = 0x2b0;
        let mut bytes = vec![0; HEADER + 16 + 128];
        let image_size = bytes.len() as u32;
        bytes[0x1c..0x20].copy_from_slice(&image_size.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&(HEADER as u32).to_le_bytes());
        bytes[HEADER..HEADER + 4].copy_from_slice(&[0, 0, 1, 0]);
        bytes[HEADER + 4..HEADER + 6].copy_from_slice(&1u16.to_le_bytes());
        bytes[HEADER + 8] = 0xb0;
        bytes[HEADER + 14..HEADER + 16].copy_from_slice(&128u16.to_le_bytes());
        let mut media = BTreeMap::from([(
            DriveId::Floppy(0),
            MediaImage::parse(MediaFormat::D88, &bytes, false).unwrap(),
        )]);
        let mut fdc = Fdc::default();
        command(&mut fdc, &mut media, &[0x06, 0, 0, 0, 1, 0, 1, 0x1b, 128]);
        for _ in 0..128 {
            fdc.read(3, &media);
        }
        assert_eq!(fdc.read(3, &media) & 0x40, 0x40);
        assert_eq!(fdc.read(3, &media), 0x20);
        assert_eq!(fdc.read(3, &media), 0x20);
    }
}
