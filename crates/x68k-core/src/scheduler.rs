//! CPUサイクルを基準に周辺イベントを決定論的に配送するスケジューラ。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Event {
    Scanline,
    HorizontalSync,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Scheduler {
    now: u64,
    clock_hz: u32,
    lines_per_frame: u16,
    high_resolution: bool,
    line_phase: u64,
    events: BTreeMap<u64, Vec<Event>>,
}

impl Scheduler {
    /// 必要な初期値と依存オブジェクトを設定し、利用可能なインスタンスを構築する。
    pub(crate) fn new(clock_hz: u32) -> Self {
        let mut scheduler = Self {
            now: 0,
            clock_hz,
            lines_per_frame: 568,
            high_resolution: false,
            line_phase: 0,
            events: BTreeMap::new(),
        };
        scheduler.schedule_line_events(0);
        scheduler
    }

    /// 内部状態をリセットし、関連する周辺機器を起動直後の状態へ戻す。
    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.clock_hz);
    }

    /// 指定された時間またはクロック分だけ状態機械を進め、発生した事象を処理する。
    pub(crate) fn advance(&mut self, cycles: u32) -> Vec<Event> {
        self.now = self.now.saturating_add(u64::from(cycles));
        let mut due = Vec::new();
        while let Some((&at, _)) = self.events.first_key_value() {
            if at > self.now {
                break;
            }
            let events = self.events.remove(&at).unwrap_or_default();
            for event in events {
                if event == Event::Scanline {
                    self.schedule_line_events(at);
                }
                due.push(event);
            }
        }
        due
    }

    /// 次のデバイスイベント境界までのCPUサイクル数。
    /// CPUはこの境界を越えない予算で実行し、イベント後のIRQを次命令へ反映する。
    pub(crate) fn cycles_until_next_event(&self) -> u32 {
        self.events
            .first_key_value()
            .map(|(&at, _)| at.saturating_sub(self.now).clamp(1, u64::from(u32::MAX)) as u32)
            .unwrap_or(u32::MAX)
    }

    /// MFP GPIP bit 7へ入力する水平同期レベル。
    ///
    /// 次のイベントが同期パルス開始なら現在はhigh、走査線終端なら現在はlow。
    /// イベント境界でCPU実行を区切るため、guestのbusy loopからも両edgeを観測できる。
    pub(crate) fn horizontal_sync_high(&self) -> bool {
        self.events
            .first_key_value()
            .and_then(|(_, events)| events.first())
            .is_none_or(|event| *event == Event::HorizontalSync)
    }

    /// 指定値を内部状態へ反映し、依存する設定や派生値も更新する。
    pub(crate) fn set_video_timing(&mut self, lines: u16, high_resolution: bool) {
        let lines = lines.max(1);
        if self.lines_per_frame != lines || self.high_resolution != high_resolution {
            self.lines_per_frame = lines;
            self.high_resolution = high_resolution;
            self.line_phase = 0;
            // 旧タイミングで予約済みの次走査線を破棄する。CRTC設定変更時点から
            // 新タイミングを適用し、1走査線分の古い周期を残さない。
            self.events.clear();
            self.schedule_line_events(self.now);
        }
    }

    /// 現在走査線の表示開始・ラスタ・同期イベントをスケジューラへ登録する。
    fn schedule_line_events(&mut self, from: u64) {
        // 映像発振器はCPUクロックと独立している。10MHz機換算の1フレーム
        // 周期を機種別CPUクロックへ有理数で換算し、端数を累積する。
        const REFERENCE_CPU_HZ: u64 = 10_000_000;
        let reference_frame_cycles = if self.high_resolution {
            180_310
        } else {
            162_707
        };
        let numerator = u64::from(self.clock_hz) * reference_frame_cycles;
        let denominator = REFERENCE_CPU_HZ * u64::from(self.lines_per_frame);
        let base = (numerator / denominator).max(1);
        self.line_phase = self.line_phase.saturating_add(numerator % denominator);
        let carry = self.line_phase / denominator;
        self.line_phase %= denominator;
        let line_cycles = base.saturating_add(carry);
        // 水平同期は走査線末尾の短いactive-low pulseとして扱う。正確な幅より、
        // GPIPをpollするソフトが両edgeを決定論的に観測できることを優先する。
        let sync_at = from.saturating_add((line_cycles.saturating_mul(7) / 8).max(1));
        self.events
            .entry(sync_at)
            .or_default()
            .push(Event::HorizontalSync);
        self.events
            .entry(from.saturating_add(line_cycles))
            .or_default()
            .push(Event::Scanline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// `preserves_fractional_scanline_cycles_without_drift` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn preserves_fractional_scanline_cycles_without_drift() {
        let mut scheduler = Scheduler::new(10_000_000);
        assert_eq!(scheduler.advance(250), vec![Event::HorizontalSync]);
        assert!(!scheduler.horizontal_sync_high());
        assert!(scheduler.advance(35).is_empty());
        assert_eq!(scheduler.advance(1), vec![Event::Scanline]);
        assert!(scheduler.horizontal_sync_high());
        let events = scheduler.advance(162_421);
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == Event::Scanline)
                .count(),
            567
        );
    }

    #[test]
    /// `a_large_slice_delivers_every_due_event` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn a_large_slice_delivers_every_due_event() {
        let mut scheduler = Scheduler::new(10_000_000);
        let events = scheduler.advance(162_707);
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == Event::Scanline)
                .count(),
            568
        );
        assert_eq!(events.len(), 568 * 2);
    }

    #[test]
    /// `exposes_exact_next_event_boundary` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn exposes_exact_next_event_boundary() {
        let mut scheduler = Scheduler::new(10_000_000);
        assert_eq!(scheduler.cycles_until_next_event(), 250);
        assert!(scheduler.advance(100).is_empty());
        assert_eq!(scheduler.cycles_until_next_event(), 150);
        assert_eq!(scheduler.advance(150), vec![Event::HorizontalSync]);
        assert_eq!(scheduler.cycles_until_next_event(), 36);
        assert_eq!(scheduler.advance(36), vec![Event::Scanline]);
        assert!(scheduler.cycles_until_next_event() >= 250);
    }

    #[test]
    /// `high_resolution_and_machine_clock_scale_without_drift` が想定する振る舞いを満たし、回帰がないことを検証する。
    fn high_resolution_and_machine_clock_scale_without_drift() {
        let mut scheduler = Scheduler::new(25_000_000);
        scheduler.set_video_timing(568, true);
        assert_eq!(
            scheduler
                .advance(450_775)
                .iter()
                .filter(|event| **event == Event::Scanline)
                .count(),
            568
        );
    }
}
