//! First-frame timestamp file written by GPU Screen Recorder (`-write-first-frame-ts`).

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FirstFrameTimestamp {
    pub monotonic_us: u64,
    pub realtime_us: u64,
}

fn is_header_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "monotonic_microsec" | "realtime_microsec"
    )
}

/// Parse the two-integer GSR timestamp document. Extra trailing whitespace is
/// allowed; any other extra tokens are rejected.
///
/// GPU Screen Recorder 6.x writes a header row (`monotonic_microsec` /
/// `realtime_microsec`) before the integer pair. A header with no numbers yet
/// is incomplete, not malformed.
pub fn parse_first_frame_timestamp(source: &str) -> Result<FirstFrameTimestamp, TimestampError> {
    let mut tokens = source.split_whitespace().peekable();
    if tokens.peek().copied().is_some_and(is_header_token) {
        let _ = tokens.next();
        if tokens.peek().copied().is_some_and(is_header_token) {
            let _ = tokens.next();
        }
    }
    let monotonic = tokens
        .next()
        .ok_or(TimestampError::MissingMonotonic)?
        .parse()
        .map_err(|_| TimestampError::InvalidMonotonic)?;
    let realtime = tokens
        .next()
        .ok_or(TimestampError::MissingRealtime)?
        .parse()
        .map_err(|_| TimestampError::InvalidRealtime)?;
    if tokens.next().is_some() {
        return Err(TimestampError::ExtraTokens);
    }
    Ok(FirstFrameTimestamp {
        monotonic_us: monotonic,
        realtime_us: realtime,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimestampError {
    MissingMonotonic,
    MissingRealtime,
    InvalidMonotonic,
    InvalidRealtime,
    ExtraTokens,
}

impl fmt::Display for TimestampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingMonotonic => {
                write!(
                    formatter,
                    "first-frame timestamp is missing the monotonic value"
                )
            }
            Self::MissingRealtime => {
                write!(
                    formatter,
                    "first-frame timestamp is missing the realtime value"
                )
            }
            Self::InvalidMonotonic => write!(
                formatter,
                "first-frame monotonic timestamp is not an integer"
            ),
            Self::InvalidRealtime => write!(
                formatter,
                "first-frame realtime timestamp is not an integer"
            ),
            Self::ExtraTokens => write!(formatter, "first-frame timestamp has extra tokens"),
        }
    }
}

impl std::error::Error for TimestampError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_integers_on_one_line() {
        let parsed = parse_first_frame_timestamp("1001 2002\n").unwrap();
        assert_eq!(parsed.monotonic_us, 1001);
        assert_eq!(parsed.realtime_us, 2002);
    }

    #[test]
    fn two_lines_are_accepted() {
        let parsed = parse_first_frame_timestamp("11\n22\n").unwrap();
        assert_eq!(parsed.monotonic_us, 11);
        assert_eq!(parsed.realtime_us, 22);
    }

    #[test]
    fn gsr6_headered_tsv_is_accepted() {
        let source =
            include_str!("../../../tests/fixtures/gsr/captured/nvidia-6.0.0/first-frame.ts");
        let parsed = parse_first_frame_timestamp(source).unwrap();
        assert_eq!(parsed.monotonic_us, 261_773_753_358);
        assert_eq!(parsed.realtime_us, 1_786_686_405_432_508);
    }

    #[test]
    fn header_without_numbers_is_incomplete() {
        assert_eq!(
            parse_first_frame_timestamp("monotonic_microsec\trealtime_microsec\n").unwrap_err(),
            TimestampError::MissingMonotonic
        );
    }

    #[test]
    fn empty_and_malformed_documents_are_rejected() {
        assert_eq!(
            parse_first_frame_timestamp("").unwrap_err(),
            TimestampError::MissingMonotonic
        );
        assert_eq!(
            parse_first_frame_timestamp("1").unwrap_err(),
            TimestampError::MissingRealtime
        );
        assert_eq!(
            parse_first_frame_timestamp("nope 2").unwrap_err(),
            TimestampError::InvalidMonotonic
        );
        assert_eq!(
            parse_first_frame_timestamp("1 2 3").unwrap_err(),
            TimestampError::ExtraTokens
        );
    }
}
