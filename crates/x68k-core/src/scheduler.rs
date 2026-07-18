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
    pub(crate) fn new(clock_hz: u32) -> Self {
        let mut scheduler = Self {
            now: 0,
            clock_hz,
            lines_per_frame: 568,
            high_resolution: false,
            line_phase: 0,
            events: BTreeMap::new(),
        };
        scheduler.schedule_next_scanline(0);
        scheduler
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new(self.clock_hz);
    }

    pub(crate) fn advance(&mut self, cycles: u32) -> Vec<Event> {
        self.now = self.now.saturating_add(u64::from(cycles));
        let mut due = Vec::new();
        loop {
            let Some((&at, _)) = self.events.first_key_value() else {
                break;
            };
            if at > self.now {
                break;
            }
            let events = self.events.remove(&at).unwrap_or_default();
            for event in events {
                if event == Event::Scanline {
                    self.schedule_next_scanline(at);
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

    pub(crate) fn set_video_timing(&mut self, lines: u16, high_resolution: bool) {
        let lines = lines.max(1);
        if self.lines_per_frame != lines || self.high_resolution != high_resolution {
            self.lines_per_frame = lines;
            self.high_resolution = high_resolution;
            self.line_phase = 0;
            // 旧タイミングで予約済みの次走査線を破棄する。CRTC設定変更時点から
            // 新タイミングを適用し、1走査線分の古い周期を残さない。
            self.events.clear();
            self.schedule_next_scanline(self.now);
        }
    }

    fn schedule_next_scanline(&mut self, from: u64) {
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
