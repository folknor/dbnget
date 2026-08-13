//! Session conventions: how a calendar date maps to a span of wall-clock time.
//!
//! Databento reads a date-only bound as UTC midnight. For a venue whose trading day
//! does not start at UTC midnight that is the wrong boundary: a CME session runs from
//! 17:00 the previous day to 16:00, America/Chicago, so chunking a request on UTC
//! midnight clips the front of each opening session and pulls in the same slice of the
//! session after it. Every bound this module produces is an explicit UTC instant
//! derived from the venue's own clock, DST included.

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
use databento::historical::DateTimeRange;
use jiff::{civil, tz::TimeZone};
use time::OffsetDateTime;

/// How a venue's trading day maps onto wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionConvention {
    /// The trading day is the UTC calendar day. Correct for venues that never close.
    Utc,
    /// The trading day opens on the previous calendar day in `tz` and closes on the
    /// named day. The daily break between `close` and the next `open` is not part of
    /// any session, so consecutive sessions do not tile the calendar.
    Overnight {
        tz: &'static str,
        open_hour: i8,
        close_hour: i8,
    },
}

/// The CME Globex trading day: 17:00 the previous day to 16:00, America/Chicago.
pub const CME: SessionConvention = SessionConvention::Overnight {
    tz: "America/Chicago",
    open_hour: 17,
    close_hour: 16,
};

/// What the user asked for on the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionArg {
    /// Pick the convention from the dataset code.
    Auto,
    /// Force UTC calendar days.
    Utc,
    /// Force the CME Globex trading day.
    Cme,
}

impl SessionArg {
    /// Resolves the flag against the dataset being requested.
    pub fn resolve(self, dataset: &str) -> SessionConvention {
        match self {
            Self::Auto => for_dataset(dataset),
            Self::Utc => SessionConvention::Utc,
            Self::Cme => CME,
        }
    }
}

/// The convention a dataset code implies.
///
/// Only the CME datasets are special-cased. Everything else falls back to UTC days,
/// which is right for 24/7 venues and no worse than the API's own default elsewhere;
/// `--session` overrides it.
pub fn for_dataset(dataset: &str) -> SessionConvention {
    if dataset.to_ascii_uppercase().starts_with("GLBX") {
        CME
    } else {
        SessionConvention::Utc
    }
}

/// Splits `range` into one chunk per trading session, clamped to the requested bounds.
///
/// Sessions that fall entirely inside the range come back whole; the first and last are
/// truncated to `range`. Time outside any session (the CME maintenance break, weekends
/// under an overnight convention) is dropped rather than requested.
pub fn split(range: &DateTimeRange, convention: SessionConvention) -> Result<Vec<DateTimeRange>> {
    match convention {
        SessionConvention::Utc => Ok(split_utc(range)),
        SessionConvention::Overnight {
            tz,
            open_hour,
            close_hour,
        } => split_overnight(range, tz, open_hour, close_hour),
    }
}

/// One chunk per UTC calendar day.
fn split_utc(range: &DateTimeRange) -> Vec<DateTimeRange> {
    let mut chunks = Vec::new();
    let mut cursor = range.start;
    while cursor < range.end {
        let next = next_utc_midnight(cursor).min(range.end);
        chunks.push(DateTimeRange::from(cursor..next));
        cursor = next;
    }
    chunks
}

/// One chunk per venue session.
fn split_overnight(
    range: &DateTimeRange,
    tz: &str,
    open_hour: i8,
    close_hour: i8,
) -> Result<Vec<DateTimeRange>> {
    let zone = TimeZone::get(tz).with_context(|| format!("no time zone named `{tz}`"))?;
    let mut chunks = Vec::new();
    let mut day = venue_date(range.start, &zone)?;

    loop {
        let (open, close) = bounds(day, &zone, open_hour, close_hour)?;
        if open >= range.end {
            break;
        }
        let start = open.max(range.start);
        let end = close.min(range.end);
        if start < end {
            chunks.push(DateTimeRange::from(start..end));
        }
        day = day
            .tomorrow()
            .map_err(|e| anyhow!("stepping past {day}: {e}"))?;
    }
    Ok(chunks)
}

/// The UTC instants a session opens and closes.
fn bounds(
    day: civil::Date,
    zone: &TimeZone,
    open_hour: i8,
    close_hour: i8,
) -> Result<(OffsetDateTime, OffsetDateTime)> {
    let previous = day
        .yesterday()
        .map_err(|e| anyhow!("stepping before {day}: {e}"))?;
    let open = to_utc(previous.at(open_hour, 0, 0, 0), zone)?;
    let close = to_utc(day.at(close_hour, 0, 0, 0), zone)?;
    Ok((open, close))
}

/// Resolves a wall-clock time in `zone` to a UTC instant, applying the zone's own DST
/// rules rather than a fixed offset.
fn to_utc(local: civil::DateTime, zone: &TimeZone) -> Result<OffsetDateTime> {
    let zoned = local.to_zoned(zone.clone()).with_context(|| {
        format!(
            "`{local}` is not a valid local time in {}",
            zone.iana_name().unwrap_or("this zone")
        )
    })?;
    let nanos = zoned.timestamp().as_nanosecond();
    OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .with_context(|| format!("`{local}` is outside the representable range"))
}

/// The venue-local calendar date an instant falls on.
fn venue_date(instant: OffsetDateTime, zone: &TimeZone) -> Result<civil::Date> {
    let timestamp = jiff::Timestamp::from_nanosecond(instant.unix_timestamp_nanos())
        .with_context(|| format!("`{instant}` is outside the representable range"))?;
    Ok(timestamp.to_zoned(zone.clone()).date())
}

fn next_utc_midnight(instant: OffsetDateTime) -> OffsetDateTime {
    (instant.date() + time::Duration::days(1))
        .midnight()
        .assume_utc()
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn utc_days_tile_the_range() {
        let range =
            DateTimeRange::from(datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-04 0:00 UTC));
        let chunks = split(&range, SessionConvention::Utc).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].start, range.start);
        assert_eq!(chunks[2].end, range.end);
    }

    #[test]
    fn cme_sessions_open_the_previous_evening() {
        // 2024-05-01 is a Wednesday in CDT (UTC-5): 17:00 CT = 22:00 UTC the day before,
        // 16:00 CT = 21:00 UTC the same day.
        let range =
            DateTimeRange::from(datetime!(2024-04-30 22:00 UTC)..datetime!(2024-05-01 21:00 UTC));
        let chunks = split(&range, CME).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start, datetime!(2024-04-30 22:00 UTC));
        assert_eq!(chunks[0].end, datetime!(2024-05-01 21:00 UTC));
    }

    #[test]
    fn cme_sessions_follow_dst() {
        // In CST (UTC-6) the same session runs 23:00 UTC to 22:00 UTC.
        let range =
            DateTimeRange::from(datetime!(2024-11-29 23:00 UTC)..datetime!(2024-11-30 22:00 UTC));
        let chunks = split(&range, CME).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start, datetime!(2024-11-29 23:00 UTC));
        assert_eq!(chunks[0].end, datetime!(2024-11-30 22:00 UTC));
    }

    #[test]
    fn cme_sessions_exclude_the_daily_break() {
        let range =
            DateTimeRange::from(datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-03 0:00 UTC));
        let chunks = split(&range, CME).unwrap();
        // Consecutive sessions leave the 16:00-17:00 CT break unrequested.
        for pair in chunks.windows(2) {
            assert!(pair[0].end < pair[1].start, "sessions must not overlap");
        }
    }

    #[test]
    fn chunks_never_escape_the_requested_range() {
        let range =
            DateTimeRange::from(datetime!(2024-05-01 8:00 UTC)..datetime!(2024-05-03 8:00 UTC));
        for chunk in split(&range, CME).unwrap() {
            assert!(chunk.start >= range.start);
            assert!(chunk.end <= range.end);
            assert!(chunk.start < chunk.end);
        }
    }

    #[test]
    fn glbx_datasets_get_the_cme_convention() {
        assert_eq!(for_dataset("GLBX.MDP3"), CME);
        assert_eq!(for_dataset("XNAS.ITCH"), SessionConvention::Utc);
    }
}
