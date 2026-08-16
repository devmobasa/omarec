use std::time::{SystemTime, UNIX_EPOCH};

use omarec_core::SessionId;

use crate::coordinator::CoordinatorClock;

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl CoordinatorClock for SystemClock {
    fn unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }

    fn session_id(&self) -> SessionId {
        SessionId::new()
    }
}

impl omarec_core::Clock for SystemClock {
    fn formatted_stamp(&self, pattern: &str) -> String {
        format_unix_utc(pattern, self.unix_ms() / 1000)
    }
}

pub fn format_unix_utc(pattern: &str, unix_secs: u64) -> String {
    let (year, month, day, hour, minute, second) = civil_utc(unix_secs);
    pattern
        .replace("%Y", &format!("{year:04}"))
        .replace("%m", &format!("{month:02}"))
        .replace("%d", &format!("{day:02}"))
        .replace("%H", &format!("{hour:02}"))
        .replace("%M", &format!("{minute:02}"))
        .replace("%S", &format!("{second:02}"))
}

fn civil_utc(unix_secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400).cast_signed();
    let rem = unix_secs % 86_400;
    let hour = u32::try_from(rem / 3600).unwrap_or(0);
    let minute = u32::try_from((rem % 3600) / 60).unwrap_or(0);
    let second = u32::try_from(rem % 60).unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, second)
}

/// Howard Hinnant's civil-from-days algorithm, Unix epoch day 0 = 1970-01-01.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_epoch_utc() {
        assert_eq!(
            format_unix_utc("%Y-%m-%d_%H-%M-%S", 0),
            "1970-01-01_00-00-00"
        );
        assert_eq!(
            format_unix_utc("%Y-%m-%d_%H-%M-%S", 86_400 + 3661),
            "1970-01-02_01-01-01"
        );
    }
}
