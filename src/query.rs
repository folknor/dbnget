//! Turns the flat CLI arguments into the parameter types the Databento client wants.

use anyhow::{Context, Result, bail};
use databento::{
    Symbols,
    historical::{
        DateTimeRange,
        batch::{JobState, SplitDuration},
        metadata::GetQueryParams,
        timeseries::GetRangeParams,
    },
};
use time::{Date, Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::cli::{CliCompression, CliEncoding, CliJobState, CliSplitDuration, QueryArgs};

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
/// a single-day pull almost always means.
pub fn date_time_range(args: &QueryArgs) -> Result<DateTimeRange> {
    let start = parse_instant(&args.start)?;
    let end = match args.end.as_deref() {
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
        bail!("at least one --symbols value is required (use ALL_SYMBOLS for everything)");
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

/// Builds the parameters for a streaming timeseries request.
pub fn range_params(args: &QueryArgs) -> Result<GetRangeParams> {
    Ok(GetRangeParams::builder()
        .dataset(&args.dataset)
        .symbols(symbols(&args.symbols)?)
        .schema(args.schema)
        .date_time_range(date_time_range(args)?)
        .stype_in(args.stype_in)
        .stype_out(args.stype_out)
        .maybe_limit(args.limit)
        .build())
}

/// Builds the parameters for the metadata endpoints that price a query.
pub fn metadata_params(args: &QueryArgs) -> Result<GetQueryParams> {
    Ok(GetQueryParams::builder()
        .dataset(&args.dataset)
        .symbols(symbols(&args.symbols)?)
        .schema(args.schema)
        .date_time_range(date_time_range(args)?)
        .stype_in(args.stype_in)
        .maybe_limit(args.limit)
        .build())
}

impl From<CliEncoding> for databento::dbn::Encoding {
    fn from(value: CliEncoding) -> Self {
        match value {
            CliEncoding::Dbn => Self::Dbn,
            CliEncoding::Csv => Self::Csv,
            CliEncoding::Json => Self::Json,
        }
    }
}

impl From<CliCompression> for databento::dbn::Compression {
    fn from(value: CliCompression) -> Self {
        match value {
            CliCompression::None => Self::None,
            CliCompression::Zstd => Self::Zstd,
        }
    }
}

impl From<CliSplitDuration> for SplitDuration {
    fn from(value: CliSplitDuration) -> Self {
        match value {
            CliSplitDuration::Day => Self::Day,
            CliSplitDuration::Week => Self::Week,
            CliSplitDuration::Month => Self::Month,
            CliSplitDuration::Year => Self::Year,
            CliSplitDuration::None => Self::None,
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
        let args = QueryArgs {
            dataset: "GLBX.MDP3".to_owned(),
            schema: databento::dbn::Schema::Trades,
            symbols: vec!["ESM4".to_owned()],
            start: "2024-05-01".to_owned(),
            end: None,
            stype_in: databento::dbn::SType::RawSymbol,
            stype_out: databento::dbn::SType::InstrumentId,
            limit: None,
        };
        let range = date_time_range(&args).unwrap();
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
        let args = QueryArgs {
            dataset: "GLBX.MDP3".to_owned(),
            schema: databento::dbn::Schema::Trades,
            symbols: vec!["ESM4".to_owned()],
            start: "2024-05-02".to_owned(),
            end: Some("2024-05-01".to_owned()),
            stype_in: databento::dbn::SType::RawSymbol,
            stype_out: databento::dbn::SType::InstrumentId,
            limit: None,
        };
        assert!(date_time_range(&args).is_err());
    }
}
