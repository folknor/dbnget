//! Parsing the flat CLI arguments into the types the Databento client wants.

use anyhow::{Context, Result, bail};
use databento::{
    Symbols,
    historical::{DateTimeRange, batch::JobState},
};
use time::{Date, Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::cli::{CliFormat, CliJobState};

/// The sentinel the Databento API uses to mean "every symbol in the dataset".
const ALL_SYMBOLS: &str = "ALL_SYMBOLS";

/// Parses `YYYY-MM-DD` or a full RFC 3339 timestamp into a UTC instant.
pub fn parse_instant(raw: &str) -> Result<OffsetDateTime> {
    if let Ok(dt) = OffsetDateTime::parse(raw, &Rfc3339) {
        return Ok(dt.to_offset(time::UtcOffset::UTC));
    }
    let date = parse_date(raw)?;
    Ok(date.midnight().assume_utc())
}

/// Parses a `YYYY-MM-DD` calendar date.
pub fn parse_date(raw: &str) -> Result<Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(raw, format)
        .with_context(|| format!("`{raw}` is not a YYYY-MM-DD date or an RFC 3339 timestamp"))
}

/// Resolves `--start` / `--end` into a half-open range.
///
/// A bare `--start` date with no `--end` covers exactly that one UTC day, which is what
/// a single-day pull almost always means. Both bounds are normalized to UTC instants so
/// that a re-run keys to the same job the first run bought.
pub fn date_time_range(start: &str, end: Option<&str>) -> Result<DateTimeRange> {
    let start = parse_instant(start)?;
    let end = match end {
        Some(raw) => parse_instant(raw)?,
        None => start + Duration::days(1),
    };
    if end <= start {
        bail!("--end ({end}) must be after --start ({start})");
    }
    Ok(DateTimeRange::from(start..end))
}

/// Builds the symbol selector, treating `ALL_SYMBOLS` and `*` as the whole dataset.
pub fn symbols(raw: &[String]) -> Result<Symbols> {
    if raw.is_empty() {
        bail!("at least one symbol is required (use ALL_SYMBOLS for everything)");
    }
    if raw
        .iter()
        .any(|s| s == "*" || s.eq_ignore_ascii_case(ALL_SYMBOLS))
    {
        if raw.len() > 1 {
            bail!("ALL_SYMBOLS cannot be combined with other symbols");
        }
        return Ok(Symbols::All);
    }
    Ok(Symbols::Symbols(raw.to_vec()))
}

impl From<CliFormat> for databento::dbn::Encoding {
    fn from(value: CliFormat) -> Self {
        match value {
            CliFormat::Dbn => Self::Dbn,
            CliFormat::Csv => Self::Csv,
            CliFormat::Json => Self::Json,
        }
    }
}

impl From<CliJobState> for JobState {
    fn from(value: CliJobState) -> Self {
        match value {
            CliJobState::Queued => Self::Queued,
            CliJobState::Processing => Self::Processing,
            CliJobState::Done => Self::Done,
            CliJobState::Expired => Self::Expired,
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    #[test]
    fn bare_date_start_covers_one_day() {
        let range = date_time_range("2024-05-01", None).unwrap();
        assert_eq!(range.end - range.start, Duration::days(1));
    }

    #[test]
    fn all_symbols_is_exclusive() {
        assert!(matches!(
            symbols(&["ALL_SYMBOLS".to_owned()]).unwrap(),
            Symbols::All
        ));
        assert!(symbols(&["ALL_SYMBOLS".to_owned(), "ESM4".to_owned()]).is_err());
        assert!(symbols(&[]).is_err());
    }

    #[test]
    fn end_must_follow_start() {
        assert!(date_time_range("2024-05-02", Some("2024-05-01")).is_err());
    }
}
