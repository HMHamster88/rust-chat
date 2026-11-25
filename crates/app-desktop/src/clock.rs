//! Local wall-clock formatting for chat timestamps.

pub(crate) fn local_clock(ts_ms: i64, clock_24h: bool) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // 100 ns ticks between 1601-01-01 and 1970-01-01.
    const EPOCH_OFFSET: i64 = 116_444_736_000_000_000;
    let ticks = ts_ms.saturating_mul(10_000).saturating_add(EPOCH_OFFSET);
    let utc_filetime = FILETIME {
        dwLowDateTime: (ticks & 0xffff_ffff) as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: both calls only read their input structs and write the output structs.
    unsafe {
        if FileTimeToSystemTime(&utc_filetime, &mut utc).is_err()
            || SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_err()
        {
            return String::new();
        }
    }
    if clock_24h {
        format_clock(local.wHour, local.wMinute, true)
    } else {
        format_clock(local.wHour, local.wMinute, false)
    }
}

fn format_clock(hour: u16, minute: u16, clock_24h: bool) -> String {
    if clock_24h {
        return format!("{hour:02}:{minute:02}");
    }
    let suffix = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        other => other,
    };
    format!("{hour}:{minute:02} {suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats_both_styles() {
        assert_eq!(format_clock(0, 5, true), "00:05");
        assert_eq!(format_clock(13, 40, true), "13:40");
        assert_eq!(format_clock(0, 5, false), "12:05 AM");
        assert_eq!(format_clock(11, 59, false), "11:59 AM");
        assert_eq!(format_clock(12, 0, false), "12:00 PM");
        assert_eq!(format_clock(23, 7, false), "11:07 PM");
    }
}
