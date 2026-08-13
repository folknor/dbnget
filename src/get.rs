//! `dbnget get` - stream a historical range straight to DBN files on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use databento::{
    HistoricalClient,
    historical::{DateTimeRange, timeseries::GetRangeParams},
};
use time::{Date, Duration, OffsetDateTime};
use tracing::info;

use crate::{cli::GetArgs, query};

/// Runs the `get` command, writing one file per day when `--daily` is set and a single
/// file for the whole range otherwise.
pub async fn run(client: &mut HistoricalClient, args: &GetArgs) -> Result<()> {
    tokio::fs::create_dir_all(&args.out)
        .await
        .with_context(|| format!("creating output directory {}", args.out.display()))?;

    let base = query::range_params(&args.query)?;
    let chunks = if args.daily {
        daily_chunks(&base.date_time_range)
    } else {
        vec![base.date_time_range.clone()]
    };

    let mut written = 0usize;
    let mut skipped = 0usize;
    for chunk in chunks {
        let path = args.out.join(file_name(&base, &chunk));
        if !args.force && is_already_downloaded(&path).await {
            info!(path = %path.display(), "already downloaded, skipping");
            skipped += 1;
            continue;
        }
        download_chunk(client, &base, chunk, &path).await?;
        written += 1;
    }

    info!(written, skipped, "download complete");
    Ok(())
}

/// Fetches one range into `path`, via a temporary file so an interrupted run never
/// leaves a truncated file that a later run would mistake for complete.
async fn download_chunk(
    client: &mut HistoricalClient,
    base: &GetRangeParams,
    range: DateTimeRange,
    path: &Path,
) -> Result<()> {
    let partial = path.with_extension("partial");
    let mut params = base.clone();
    params.date_time_range = range;

    info!(path = %path.display(), "downloading");
    let decoder = client
        .timeseries()
        .get_range_to_file(&params.with_path(&partial))
        .await
        .with_context(|| format!("downloading {}", path.display()))?;
    drop(decoder);

    tokio::fs::rename(&partial, path)
        .await
        .with_context(|| format!("finalizing {}", path.display()))?;
    Ok(())
}

/// A file counts as downloaded when it exists and holds more than an empty DBN frame.
async fn is_already_downloaded(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) => meta.len() > 0,
        Err(_) => false,
    }
}

/// Splits a range into half-open one-UTC-day chunks, clamping the last chunk to `end`.
fn daily_chunks(range: &DateTimeRange) -> Vec<DateTimeRange> {
    let mut chunks = Vec::new();
    let mut cursor = range.start;
    while cursor < range.end {
        let next = midnight_after(cursor).min(range.end);
        chunks.push(DateTimeRange::from(cursor..next));
        cursor = next;
    }
    chunks
}

/// The first UTC midnight strictly after `instant`.
fn midnight_after(instant: OffsetDateTime) -> OffsetDateTime {
    (instant.date() + Duration::days(1)).midnight().assume_utc()
}

/// Names a file after the query it holds, so a directory of them stays self-describing.
fn file_name(base: &GetRangeParams, range: &DateTimeRange) -> PathBuf {
    let dataset = base.dataset.replace('.', "_");
    let stamp = stamp(range);
    PathBuf::from(format!("{dataset}.{}.{stamp}.dbn.zst", base.schema))
}

/// A compact, sortable label for a range: the date alone when the range is a whole UTC
/// day, otherwise both endpoints.
fn stamp(range: &DateTimeRange) -> String {
    let start = format_date(range.start.date());
    if is_whole_day(range) {
        return start;
    }
    format!("{start}_{}", format_date(range.end.date()))
}

fn is_whole_day(range: &DateTimeRange) -> bool {
    range.start == range.start.date().midnight().assume_utc()
        && range.end == midnight_after(range.start)
}

fn format_date(date: Date) -> String {
    let format = time::macros::format_description!("[year][month][day]");
    date.format(format).unwrap_or_else(|_| date.to_string())
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
    fn daily_chunks_cover_the_range_exactly() {
        let range =
            DateTimeRange::from(datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-04 0:00 UTC));
        let chunks = daily_chunks(&range);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].start, range.start);
        assert_eq!(chunks.last().unwrap().end, range.end);
    }

    #[test]
    fn daily_chunks_clamp_a_partial_final_day() {
        let range =
            DateTimeRange::from(datetime!(2024-05-01 12:00 UTC)..datetime!(2024-05-02 6:00 UTC));
        let chunks = daily_chunks(&range);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].end, datetime!(2024-05-02 0:00 UTC));
        assert_eq!(chunks[1].end, range.end);
    }

    #[test]
    fn whole_day_ranges_get_a_single_date_stamp() {
        let day =
            DateTimeRange::from(datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-02 0:00 UTC));
        assert_eq!(stamp(&day), "20240501");

        let span =
            DateTimeRange::from(datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-04 0:00 UTC));
        assert_eq!(stamp(&span), "20240501_20240504");
    }
}
