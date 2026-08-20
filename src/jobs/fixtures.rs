//! Test fixtures shared by the matcher and the listing.
//!
//! Both structs are built as LITERALS, never through a builder or `..Default::default()`.
//! That is the point of this module: a field added to `SubmitJobParams` upstream that
//! nobody adds to [`super::job_matches`] is a silent double charge, so it has to fail
//! the build here first. Loosening either literal disarms that.

use databento::{
    Symbols,
    dbn::{Compression, Encoding, SType, Schema},
    historical::{
        DateTimeRange,
        batch::{BatchJob, Delivery, JobState, SplitDuration, SubmitJobParams},
    },
};
use time::macros::datetime;

use super::effective_map_symbols;

/// A submission with every field at the default the fetch path builds, mutated by
/// the caller.
pub(super) fn params(edit: impl FnOnce(&mut SubmitJobParams)) -> SubmitJobParams {
    let mut params = SubmitJobParams {
        dataset: "GLBX.MDP3".to_owned(),
        symbols: Symbols::Symbols(vec!["ESM4".to_owned()]),
        schema: Schema::Trades,
        date_time_range: DateTimeRange::from(
            datetime!(2024-05-01 0:00 UTC)..datetime!(2024-05-02 0:00 UTC),
        ),
        encoding: Encoding::Dbn,
        compression: Compression::Zstd,
        pretty_px: false,
        pretty_ts: false,
        map_symbols: None,
        split_symbols: false,
        split_duration: SplitDuration::Day,
        split_size: None,
        delivery: Delivery::Download,
        stype_in: SType::RawSymbol,
        stype_out: SType::InstrumentId,
        limit: None,
    };
    edit(&mut params);
    params
}

/// The job the vendor would echo back for `params`, with its defaults resolved the
/// way the vendor resolves them.
pub(super) fn job_from(params: &SubmitJobParams) -> BatchJob {
    BatchJob {
        id: "GLBX-20260813-TESTJOB".to_owned(),
        user_id: None,
        cost_usd: Some(0.0),
        dataset: params.dataset.clone(),
        symbols: params.symbols.clone(),
        stype_in: params.stype_in,
        stype_out: params.stype_out,
        schema: params.schema,
        start: params.date_time_range.start,
        end: params.date_time_range.end,
        limit: params.limit,
        encoding: params.encoding,
        compression: params.compression,
        pretty_px: params.pretty_px,
        pretty_ts: params.pretty_ts,
        map_symbols: effective_map_symbols(params),
        split_symbols: params.split_symbols,
        split_duration: params.split_duration,
        split_size: params.split_size,
        delivery: params.delivery,
        record_count: Some(1_000),
        billed_size: Some(1_000),
        actual_size: Some(1_000),
        package_size: Some(1_000),
        state: JobState::Done,
        ts_received: datetime!(2024-05-02 1:00 UTC),
        ts_queued: None,
        ts_process_start: None,
        ts_process_done: None,
        ts_expiration: None,
        progress: Some(100),
    }
}
