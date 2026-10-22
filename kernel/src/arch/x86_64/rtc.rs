// SPDX-License-Identifier: Apache-2.0
//! CMOS real-time clock, read once to anchor `CLOCK_REALTIME`.
//!
//! The RTC has one-second resolution and no time zone; firmware and QEMU keep
//! it in UTC. The kernel reads it once, on first use, and derives wall time
//! from the timer tick count after that, so the date never jumps backwards.

use super::timer::InterruptGuard;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

const CMOS_INDEX: u16 = 0x70;
const CMOS_DATA: u16 = 0x71;
/// Setting bit 7 of the index port keeps NMI disabled during the access.
const NMI_DISABLE: u8 = 0x80;
/// Status A bit 7: an update is in progress and the time registers are not
/// stable.
const UPDATE_IN_PROGRESS: u8 = 0x80;
/// Status B bit 1: 24-hour mode. Bit 2: binary (not BCD) values.
const HOUR_24: u8 = 0x02;
const BINARY: u8 = 0x04;
/// Reads attempted before the RTC is treated as absent.
const MAX_ATTEMPTS: usize = 1000;

const STATE_UNREAD: u8 = 0;
const STATE_VALID: u8 = 1;
const STATE_ABSENT: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(STATE_UNREAD);
/// Unix seconds at tick zero.
static BOOT_EPOCH: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Reading {
    second: u8,
    minute: u8,
    hour: u8,
    day: u8,
    month: u8,
    year: u8,
}

fn cmos(register: u8) -> u8 {
    let value: u8;
    // SAFETY: Ports 0x70/0x71 are the standard CMOS index/data pair. The
    // caller holds an interrupt guard so the index write and the data read
    // are not split.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") CMOS_INDEX, in("al") register | NMI_DISABLE, options(nomem, nostack, preserves_flags));
        core::arch::asm!("in al, dx", in("dx") CMOS_DATA, out("al") value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_once() -> Option<Reading> {
    for _ in 0..MAX_ATTEMPTS {
        if cmos(0x0a) & UPDATE_IN_PROGRESS == 0 {
            return Some(Reading {
                second: cmos(0x00),
                minute: cmos(0x02),
                hour: cmos(0x04),
                day: cmos(0x07),
                month: cmos(0x08),
                year: cmos(0x09),
            });
        }
    }
    None
}

fn from_bcd(value: u8) -> u8 {
    (value >> 4) * 10 + (value & 0x0f)
}

/// Days from 1970-01-01 to the given civil date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Reads the RTC as Unix seconds, or `None` when it is absent or reports an
/// impossible date.
fn read_unix_seconds() -> Option<u64> {
    // Read until two consecutive readings agree, so an update between
    // registers cannot produce a torn value.
    let mut previous = read_once()?;
    let mut reading = None;
    for _ in 0..MAX_ATTEMPTS {
        let next = read_once()?;
        if next == previous {
            reading = Some(next);
            break;
        }
        previous = next;
    }
    let mut value = reading?;
    let status_b = cmos(0x0b);
    let pm = value.hour & 0x80 != 0;
    value.hour &= 0x7f;
    if status_b & BINARY == 0 {
        value.second = from_bcd(value.second);
        value.minute = from_bcd(value.minute);
        value.hour = from_bcd(value.hour);
        value.day = from_bcd(value.day);
        value.month = from_bcd(value.month);
        value.year = from_bcd(value.year);
    }
    if status_b & HOUR_24 == 0 {
        // 12-hour mode: 12 AM is hour 0 and 12 PM is hour 12.
        value.hour %= 12;
        if pm {
            value.hour += 12;
        }
    }
    if value.second > 59
        || value.minute > 59
        || value.hour > 23
        || !(1..=31).contains(&value.day)
        || !(1..=12).contains(&value.month)
        || value.year > 99
    {
        return None;
    }
    // The century register is not at a fixed index, so two-digit years are
    // read as 1970..=2069.
    let year = if value.year < 70 {
        2000 + i64::from(value.year)
    } else {
        1900 + i64::from(value.year)
    };
    let days = days_from_civil(year, i64::from(value.month), i64::from(value.day));
    let seconds = days * 86_400
        + i64::from(value.hour) * 3600
        + i64::from(value.minute) * 60
        + i64::from(value.second);
    u64::try_from(seconds).ok()
}

/// Returns the Unix time at tick zero, reading the RTC on first use.
///
/// `uptime_seconds` is the current uptime, used to anchor the reading.
/// Returns `None` when no usable RTC exists.
pub fn boot_epoch(uptime_seconds: u64) -> Option<u64> {
    match STATE.load(Ordering::Acquire) {
        STATE_VALID => return Some(BOOT_EPOCH.load(Ordering::Relaxed)),
        STATE_ABSENT => return None,
        _ => {}
    }
    let _guard = InterruptGuard::enter();
    if STATE.load(Ordering::Acquire) == STATE_UNREAD {
        match read_unix_seconds() {
            Some(now) => {
                BOOT_EPOCH.store(now.saturating_sub(uptime_seconds), Ordering::Relaxed);
                STATE.store(STATE_VALID, Ordering::Release);
            }
            None => STATE.store(STATE_ABSENT, Ordering::Release),
        }
    }
    (STATE.load(Ordering::Acquire) == STATE_VALID).then(|| BOOT_EPOCH.load(Ordering::Relaxed))
}
