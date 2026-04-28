use std::sync::atomic::Ordering;

use esp_idf_svc::hal::{delay::BLOCK, i2c::I2cDriver};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

use crate::board;
use cyberkey_core::bcd::{bcd2dec, dec2bcd};

/// Set by `cmd_sync_clock` in the CLI task; drained by the main loop which
/// has exclusive access to the I2C bus and can write it to the BM8563.
pub(crate) static PENDING_RTC_WRITE: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// UTC offset in seconds (e.g. 7200 for UTC+2). Set by `sync_clock`; applied
/// in `format_time()` so the display shows local time rather than UTC.
pub(crate) static UTC_OFFSET_SECS: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(0);

pub fn write(i2c: &mut I2cDriver, ts: u64) {
    let Ok(dt) = OffsetDateTime::from_unix_timestamp(ts as i64) else {
        log::warn!("write_rtc: invalid timestamp {}", ts);
        return;
    };
    let buf = [
        0x02u8,
        dec2bcd(dt.second()),
        dec2bcd(dt.minute()),
        dec2bcd(dt.hour()),
        dec2bcd(dt.day()),
        0x00,
        dec2bcd(dt.month() as u8),
        dec2bcd((dt.year() as u16 - 2000) as u8),
    ];
    match i2c.write(board::RTC_I2C_ADDR, &buf, BLOCK) {
        Ok(()) => log::info!(
            "RTC written: {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            dt.year(),
            dt.month() as u8,
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        ),
        Err(e) => log::warn!("RTC write failed: {:?}", e),
    }
}

pub fn init(i2c: &mut I2cDriver) -> anyhow::Result<()> {
    let mut buf = [0u8; 7];
    if let Err(e) = i2c.write_read(board::RTC_I2C_ADDR, &[0x02], &mut buf, BLOCK) {
        log::warn!("RTC read failed: {:?}, using compile time", e);
        return fallback_time();
    }

    let vl = buf[0] & 0x80;
    if vl != 0 {
        log::warn!("RTC voltage low (unset), syncing RTC from compile time");
        let ts: u64 = env!("BUILD_TIME").parse().unwrap_or(0);
        set_system_time(ts);
        write(i2c, ts); // clears VL bit so next boot reads a valid time
        return Ok(());
    }

    let sec = bcd2dec(buf[0] & 0x7F);
    let min = bcd2dec(buf[1] & 0x7F);
    let hour = bcd2dec(buf[2] & 0x3F);
    let day = bcd2dec(buf[3] & 0x3F);
    let month = bcd2dec(buf[5] & 0x1F);
    let year = 2000i32 + bcd2dec(buf[6]) as i32;

    let ts = Month::try_from(month)
        .ok()
        .and_then(|m| Date::from_calendar_date(year, m, day).ok())
        .and_then(|d| Time::from_hms(hour, min, sec).ok().map(|t| (d, t)))
        .map(|(d, t)| PrimitiveDateTime::new(d, t).assume_utc().unix_timestamp() as u64);

    match ts {
        Some(ts) => {
            log::info!(
                "RTC: {:04}-{:02}-{:02} {:02}:{:02}:{:02} -> ts={}",
                year,
                month,
                day,
                hour,
                min,
                sec,
                ts
            );
            set_system_time(ts);
            Ok(())
        }
        None => {
            log::warn!("RTC: invalid date/time in registers, using fallback");
            fallback_time()
        }
    }
}

pub fn format_time() -> String {
    let utc = unsafe { esp_idf_svc::sys::time(std::ptr::null_mut()) } as i64;
    let local_ts = utc + UTC_OFFSET_SECS.load(Ordering::Relaxed) as i64;
    match OffsetDateTime::from_unix_timestamp(local_ts.max(0)) {
        Ok(dt) => format!("{:02}:{:02}", dt.hour(), dt.minute()),
        Err(_) => "??:??".to_string(),
    }
}

fn fallback_time() -> anyhow::Result<()> {
    let ts: u64 = env!("BUILD_TIME").parse().unwrap_or(0);
    log::info!("Fallback to compile time: {}", ts);
    set_system_time(ts);
    Ok(())
}

fn set_system_time(ts: u64) {
    let tv = esp_idf_svc::sys::timeval {
        tv_sec: ts as _,
        tv_usec: 0,
    };
    unsafe {
        esp_idf_svc::sys::settimeofday(&tv, std::ptr::null());
    }
}
