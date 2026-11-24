//! Shared screens, widgets and theme for desktop and Android. The toolkit is chosen by the M0
//! spike (FerroUI first, with a pre-agreed fallback); this crate keeps toolkit-independent
//! formatting helpers so both apps behave the same.

pub const APP_NAME: &str = "rust-chat";

/// `14:32` or `2:32 PM`. `offset_minutes` is the local offset from UTC, passed by the UI layer
/// so this stays dependency-free and testable.
pub fn format_timestamp(epoch_ms: i64, offset_minutes: i32, clock_24h: bool) -> String {
    let seconds = (epoch_ms.div_euclid(1000) + i64::from(offset_minutes) * 60).rem_euclid(86_400);
    let minutes_of_day = (seconds / 60) as u32;
    let hour = minutes_of_day / 60;
    let minute = minutes_of_day % 60;
    if clock_24h {
        format!("{hour:02}:{minute:02}")
    } else {
        let suffix = if hour < 12 { "AM" } else { "PM" };
        let hour12 = match hour % 12 {
            0 => 12,
            value => value,
        };
        format!("{hour12}:{minute:02} {suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_both_clock_styles() {
        // 2026-01-01T00:00:00Z
        let midnight = 1_767_225_600_000;
        assert_eq!(format_timestamp(midnight, 0, true), "00:00");
        assert_eq!(format_timestamp(midnight, 0, false), "12:00 AM");
        let afternoon = midnight + (14 * 60 + 5) * 60_000;
        assert_eq!(format_timestamp(afternoon, 0, true), "14:05");
        assert_eq!(format_timestamp(afternoon, 0, false), "2:05 PM");
    }

    #[test]
    fn applies_the_local_offset() {
        let midnight = 1_767_225_600_000;
        assert_eq!(format_timestamp(midnight, 120, true), "02:00");
        assert_eq!(format_timestamp(midnight, -60, true), "23:00");
    }
}
