//! `dbnget get` - stream a historical range straight to DBN files on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use databento::{
    HistoricalClient,
    historical::{DateTimeRange, metadata::GetRecordCountParams, timeseries::GetRangeParams},
};
use time::{OffsetDateTime, format_description::FormatItem, macros::format_description};
use tracing::info;

use crate::{Outcome, cli::GetArgs, query, session};

/// The stamp format for file names: sortable, and free of separators that would
/// collide with the dotted fields around it.
const STAMP: &[FormatItem<'_>] = format_description!("[year][month][day]T[hour][minute]");

/// Runs the `get` command: one request and one file per session when `--split` is set,
/// a single request for the whole range otherwise.
pub async fn run(client: &mut HistoricalClient, args: &GetArgs) -> Result<Outcome> {
    tokio::fs::create_dir_all(&args.out)
        .await
        .with_context(|| format!("creating output directory {}", args.out.display()))?;

    let base = query::range_params(&args.query)?;
    let chunks = plan_chunks(args, &base)?;

    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut empty = 0usize;
    for chunk in chunks {
        let path = args.out.join(file_name(&base, &chunk));
        if !args.force && is_already_downloaded(&path).await {
            info!(path = %path.display(), "already downloaded, skipping");
            skipped += 1;
            continue;
        }
        if args.skip_empty && is_empty(client, &base, &chunk).await? {
            info!(path = %path.display(), "no records in range, skipping");
            empty += 1;
            continue;
        }
        download_chunk(client, &base, chunk, &path).await?;
        written += 1;
    }

    info!(written, skipped, empty, "download complete");
    Ok(Outcome::Settled)
}

/// Splits the request into the chunks that will each become one file.
fn plan_chunks(args: &GetArgs, base: &GetRangeParams) -> Result<Vec<DateTimeRange>> {
    if !args.split {
        return Ok(vec![base.date_time_range.clone()]);
    }
    let convention = args.session.resolve(&base.dataset);
    info!(?convention, "splitting range by session");
    session::split(&base.date_time_range, convention)
}

/// Asks the (unbilled) record-count endpoint whether a chunk would return anything.
///
/// Weekends, holidays and half-sessions fall out of this without a holiday table: a
/// closure returns zero for the exact query about to be issued. Note that
/// `metadata.get_dataset_condition` cannot be used for this - it reports dataset
/// ingestion status and answers "available" for Christmas Day and every Saturday.
async fn is_empty(
    client: &mut HistoricalClient,
    base: &GetRangeParams,
    range: &DateTimeRange,
) -> Result<bool> {
    let params = GetRecordCountParams::builder()
        .dataset(&base.dataset)
        .symbols(base.symbols.clone())
        .schema(base.schema)
        .date_time_range(range.clone())
        .stype_in(base.stype_in)
        .maybe_limit(base.limit)
        .build();
    let count = client
        .metadata()
        .get_record_count(&params)
        .await
        .context("fetching record count")?;
    Ok(count == 0)
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

/// Names a file after the query it holds, so a directory of them stays self-describing.
///
/// Both bounds are always present: under a session convention a chunk is not a calendar
/// day, and a name that implied otherwise would be a lie about what is in the file.
fn file_name(base: &GetRangeParams, range: &DateTimeRange) -> PathBuf {
    let dataset = base.dataset.replace('.', "_");
    let start = stamp(range.start);
    let end = stamp(range.end);
    PathBuf::from(format!("{dataset}.{}.{start}-{end}.dbn.zst", base.schema))
}

fn stamp(instant: OffsetDateTime) -> String {
    instant
        .format(STAMP)
        .unwrap_or_else(|_| instant.unix_timestamp().to_string())
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
    fn file_names_carry_both_bounds() {
        let base = GetRangeParams::builder()
            .dataset("GLBX.MDP3")
            .symbols(vec!["ESM4".to_owned()])
            .schema(databento::dbn::Schema::Trades)
            .date_time_range(DateTimeRange::from(
                datetime!(2024-04-30 22:00 UTC)..datetime!(2024-05-01 21:00 UTC),
            ))
            .build();
        let name = file_name(&base, &base.date_time_range);
        assert_eq!(
            name.to_str().unwrap(),
            "GLBX_MDP3.trades.20240430T2200-20240501T2100.dbn.zst"
        );
    }
}
