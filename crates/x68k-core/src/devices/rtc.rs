//! RP5C15 互換 RTC。ホスト時刻に依存せず、CPUクロックから決定論的に進む。

use serde::{Deserialize, Serialize};

const MODE_TIMER_ENABLE: u8 = 0x08;
const MODE_ALARM_ENABLE: u8 = 0x04;
const MODE_BANK: u8 = 0x01;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Rtc {
    /// 1980-01-01 00:00:00 からの経過秒。
    seconds: u64,
    cycles: u64,
    subsecond_cycles: u64,
    mode: u8,
    test: u8,
    reset: u8,
    bank1: [u8; 13],
    weekday_offset: u8,
}

impl Default for Rtc {
    fn default() -> Self {
        Self {
            seconds: 0,
            cycles: 0,
            subsecond_cycles: 0,
            // 実機の電池バックアップ状態と同様、時計は初期状態から進める。
            mode: MODE_TIMER_ENABLE,
            test: 0,
            // 1Hz/16Hz端子出力は無効。alarm一致だけを割り込みへ接続する。
            reset: 0x0c,
            bank1: [0; 13],
            weekday_offset: 0,
        }
    }
}

impl Rtc {
    pub(crate) fn read(&self, offset: u32) -> u8 {
        if offset > 0x1f || offset & 1 == 0 {
            return 0;
        }
        let register = (offset >> 1) as usize;
        match register {
            13 => self.mode,
            14 => self.test,
            15 => self.reset,
            _ if self.mode & MODE_BANK != 0 => self.read_bank1(register),
            _ => self.read_clock(register),
        }
    }

    pub(crate) fn write(&mut self, offset: u32, value: u8) {
        if offset > 0x1f || offset & 1 == 0 {
            return;
        }
        let register = (offset >> 1) as usize;
        let value = value & 0x0f;
        match register {
            13 => self.mode = value & 0x0d,
            14 => self.test = value,
            15 => {
                self.reset = value;
                if value & 1 != 0 {
                    self.bank1[2..=8].fill(0);
                }
                if value & 2 != 0 {
                    self.cycles = 0;
                    self.subsecond_cycles = 0;
                }
            }
            _ if self.mode & MODE_BANK != 0 => {
                if register < self.bank1.len() && register != 11 {
                    self.bank1[register] = value;
                }
            }
            0..=12 => self.write_clock(register, value),
            _ => {}
        }
    }

    pub(crate) fn tick(&mut self, cycles: u32, clock_hz: u32) -> bool {
        let clock = u64::from(clock_hz.max(1));
        self.subsecond_cycles = self.subsecond_cycles.saturating_add(u64::from(cycles));
        let sixteenth = (clock / 16).max(1);
        let mut interrupt = false;
        while self.subsecond_cycles >= sixteenth {
            self.subsecond_cycles -= sixteenth;
            interrupt |= self.reset & 4 == 0;
        }

        if self.mode & MODE_TIMER_ENABLE == 0 {
            return interrupt;
        }
        self.cycles = self.cycles.saturating_add(u64::from(cycles));
        while self.cycles >= clock {
            self.cycles -= clock;
            self.seconds = self.seconds.saturating_add(1);
            interrupt |= self.reset & 8 == 0;
            interrupt |= self.mode & MODE_ALARM_ENABLE != 0 && self.alarm_matches();
        }
        interrupt
    }

    fn read_clock(&self, register: usize) -> u8 {
        let (year, month, day, weekday, mut hour, minute, second) = self.calendar();
        let twenty_four_hour = self.bank1[10] & 1 != 0;
        if !twenty_four_hour {
            let pm = hour >= 12;
            hour %= 12;
            if hour == 0 {
                hour = 12;
            }
            if register == 5 {
                return hour / 10 | if pm { 2 } else { 0 };
            }
        }
        match register {
            0 => second % 10,
            1 => second / 10,
            2 => minute % 10,
            3 => minute / 10,
            4 => hour % 10,
            5 => hour / 10,
            6 => weekday,
            7 => day % 10,
            8 => day / 10,
            9 => month % 10,
            10 => month / 10,
            11 => ((year - 1980) % 10) as u8,
            12 => (((year - 1980) / 10) % 10) as u8,
            _ => 0,
        }
    }

    fn read_bank1(&self, register: usize) -> u8 {
        match register {
            11 => ((self.calendar().0 - 1980) & 3) as u8,
            0..=12 => self.bank1[register],
            _ => 0,
        }
    }

    fn write_clock(&mut self, register: usize, value: u8) {
        let (mut year, mut month, mut day, weekday, mut hour, mut minute, mut second) =
            self.calendar();
        match register {
            0 => second = second / 10 * 10 + value.min(9),
            1 => second = value.min(5) * 10 + second % 10,
            2 => minute = minute / 10 * 10 + value.min(9),
            3 => minute = value.min(5) * 10 + minute % 10,
            4 => hour = hour / 10 * 10 + value.min(9),
            5 => {
                if self.bank1[10] & 1 != 0 {
                    hour = value.min(2) * 10 + hour % 10;
                } else {
                    let display = (value & 1) * 10 + hour % 10;
                    hour = display % 12 + if value & 2 != 0 { 12 } else { 0 };
                }
            }
            6 => {
                let natural = natural_weekday(year, month, day);
                self.weekday_offset = (value.min(6) + 7 - natural) % 7;
                return;
            }
            7 => day = day / 10 * 10 + value.min(9),
            8 => day = value.min(3) * 10 + day % 10,
            9 => month = month / 10 * 10 + value.min(9),
            10 => month = value.min(1) * 10 + month % 10,
            11 => year = 1980 + (year - 1980) / 10 * 10 + u16::from(value.min(9)),
            12 => year = 1980 + u16::from(value.min(9)) * 10 + (year - 1980) % 10,
            _ => return,
        }
        month = month.clamp(1, 12);
        day = day.clamp(1, month_length(year, month));
        hour = hour.min(23);
        minute = minute.min(59);
        second = second.min(59);
        self.seconds = seconds_from_calendar(year, month, day, hour, minute, second);
        // 日付変更では曜日の明示設定を維持する。
        let natural = natural_weekday(year, month, day);
        self.weekday_offset = (weekday + 7 - natural) % 7;
    }

    fn alarm_matches(&self) -> bool {
        // RP5C15のalarm比較対象は分・時・曜日・日。各BCD桁をそのまま比較する。
        (2..=8).all(|register| self.bank1[register] == self.read_clock(register))
    }

    fn calendar(&self) -> (u16, u8, u8, u8, u8, u8, u8) {
        let second = (self.seconds % 60) as u8;
        let minute = (self.seconds / 60 % 60) as u8;
        let hour = (self.seconds / 3600 % 24) as u8;
        let mut days = self.seconds / 86_400;
        let natural_weekday = ((2 + days) % 7) as u8; // 1980-01-01は火曜日。
        let weekday = (natural_weekday + self.weekday_offset) % 7;
        let mut year = 1980u16;
        loop {
            let length = if leap(year) { 366 } else { 365 };
            if days < length {
                break;
            }
            days -= length;
            year += 1;
        }
        let mut month = 1u8;
        while days >= u64::from(month_length(year, month)) {
            days -= u64::from(month_length(year, month));
            month += 1;
        }
        (year, month, days as u8 + 1, weekday, hour, minute, second)
    }
}

fn seconds_from_calendar(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u64 {
    let mut days = 0u64;
    for current in 1980..year {
        days += if leap(current) { 366 } else { 365 };
    }
    for current in 1..month {
        days += u64::from(month_length(year, current));
    }
    days += u64::from(day.saturating_sub(1));
    days * 86_400 + u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second)
}

fn natural_weekday(year: u16, month: u8, day: u8) -> u8 {
    ((2 + seconds_from_calendar(year, month, day, 0, 0, 0) / 86_400) % 7) as u8
}

fn month_length(year: u16, month: u8) -> u8 {
    match month {
        2 if leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn leap(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_deterministically_from_1980_epoch() {
        let mut rtc = Rtc::default();
        rtc.tick(10_000_000, 10_000_000);
        assert_eq!(rtc.read(1), 1);
        assert_eq!(rtc.read(0x0d), 2);
        assert_eq!(rtc.read(0x0f), 1);
    }

    #[test]
    fn bank_date_writes_and_timer_stop_follow_rp5c15_registers() {
        let mut rtc = Rtc::default();
        rtc.write(0x1b, MODE_BANK);
        rtc.write(0x15, 1); // 24-hour mode
        rtc.write(0x1b, 0); // bank 0, timer stop
        rtc.write(0x01, 8);
        rtc.write(0x03, 5);
        rtc.write(0x05, 9);
        rtc.write(0x07, 5);
        rtc.write(0x09, 3);
        rtc.write(0x0b, 2);
        rtc.write(0x0f, 9);
        rtc.write(0x11, 2);
        rtc.write(0x13, 2);
        rtc.write(0x15, 0);
        rtc.write(0x17, 4);
        rtc.write(0x19, 4); // 2024-02-29 23:59:58
        rtc.tick(20_000_000, 10_000_000);
        assert_eq!((rtc.read(0x03), rtc.read(0x01)), (5, 8));

        rtc.write(0x1b, MODE_TIMER_ENABLE);
        rtc.tick(20_000_000, 10_000_000);
        assert_eq!((rtc.read(0x15), rtc.read(0x13)), (0, 3));
        assert_eq!((rtc.read(0x11), rtc.read(0x0f)), (0, 1));
    }

    #[test]
    fn bank_one_exposes_24_hour_and_leap_year_registers() {
        let mut rtc = Rtc::default();
        rtc.write(0x1b, MODE_TIMER_ENABLE | MODE_BANK);
        rtc.write(0x15, 1);
        assert_eq!(rtc.read(0x15), 1);
        assert_eq!(rtc.read(0x17), 0);
        assert_eq!(rtc.read(0x1b), MODE_TIMER_ENABLE | MODE_BANK);
    }
}
