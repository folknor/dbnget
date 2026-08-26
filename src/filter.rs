//! The `dbnget list` selectors.
//!
//! # Why these never consult the request index
//!
//! It is tempting: the index already holds dataset, schema and symbols for every job it
//! has seen, so filtering against it locally would turn a filtered listing into almost
//! no requests at all. It is also wrong, for two reasons that took a review to see
//! clearly.
//!
//! `RequestKey` is documented as safe when under-specified, because a missing field
//! makes it propose an EXTRA candidate that live confirmation then rejects - every
//! direction it can be wrong in is a performance bug. That property holds only while it
//! is used to say "look here first". The moment it is used to say "this row does not
//! match", an omitted field or a stale entry becomes a row that silently vanishes from a
//! listing, and the documentation on that type becomes false.
//!
//! And `dbnget list` is the DIAGNOSTIC surface - where someone goes to work out why a
//! request did not adopt an existing job. Letting a hint answer that question means a
//! user investigating a suspected non-adoption can run the exact selector for it and be
//! told `no jobs`, because the same local guess implicated in the reconciliation also
//! hid the evidence. Re-checking against live records afterwards cannot repair this: it
//! prevents false positives, and a row dropped before its fetch was never a positive.
//!
//! So selection is exact. Filters run against live records only, and the cost is
//! managed by fetching newest-first and stopping once `--limit` is satisfied, rather
//! than by guessing which rows are worth fetching.

use anyhow::{Context, Result};
use databento::historical::batch::BatchJob;
use regex::{Regex, RegexBuilder};

use crate::cli::ListArgs;

/// A compiled set of selectors, or nothing to select on.
#[derive(Debug, Default)]
pub struct Selectors {
    dataset: Vec<Regex>,
    schema: Vec<Regex>,
    symbol: Vec<Regex>,
    job_id: Vec<Regex>,
}

impl Selectors {
    /// Compiles every pattern up front.
    ///
    /// Before any network call, deliberately: a typo in a pattern should cost nothing
    /// and be reported as what it is, rather than surfacing after a listing has already
    /// been fetched.
    pub fn compile(args: &ListArgs) -> Result<Self> {
        Ok(Self {
            dataset: compile_all("--dataset", &args.dataset)?,
            schema: compile_all("--schema", &args.schema)?,
            symbol: compile_all("--symbol", &args.symbol)?,
            job_id: compile_all("--job-id", &args.job_id)?,
        })
    }

    /// Whether any selector needs a field the short listing does not carry.
    ///
    /// The short record holds id, state and received-time, so `--job-id` can be answered
    /// without spending a request and the other three cannot. Keeping that distinction
    /// explicit is what lets a job-id-only listing skip the fan-out entirely.
    pub fn needs_details(&self) -> bool {
        !(self.dataset.is_empty() && self.schema.is_empty() && self.symbol.is_empty())
    }

    /// Whether a job id passes, judged from the short listing alone.
    pub fn accepts_id(&self, id: &str) -> bool {
        any_matches(&self.job_id, id)
    }

    /// Whether a live job passes every selector.
    ///
    /// Symbols are matched ONE AT A TIME against the split list, never against a joined
    /// string. Joining first would let a pattern match across the boundary between two
    /// symbols, or match the separator itself, so `--symbol 'T,M'` would select a job
    /// holding `ES.FUT` and `MES.FUT` and nothing about that job contains that symbol.
    pub fn accepts(&self, job: &BatchJob) -> bool {
        self.accepts_id(&job.id)
            && any_matches(&self.dataset, &job.dataset)
            && any_matches(&self.schema, job.schema.as_str())
            && (self.symbol.is_empty()
                || crate::jobs::symbol_names(&job.symbols)
                    .iter()
                    .any(|name| any_matches(&self.symbol, name)))
    }
}

/// A dimension with no patterns imposes no condition; otherwise ANY pattern matching is
/// enough. Repeating a flag is therefore OR, and combining different flags is AND.
fn any_matches(patterns: &[Regex], value: &str) -> bool {
    patterns.is_empty() || patterns.iter().any(|re| re.is_match(value))
}

fn compile_all(flag: &str, patterns: &[String]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pattern| {
            RegexBuilder::new(pattern)
                // Datasets are upper case, schemas lower, symbols upper. Nobody should
                // have to remember which to match one.
                .case_insensitive(true)
                .build()
                .with_context(|| format!("{flag} pattern `{pattern}` is not a valid regex"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use databento::{Symbols, dbn::Schema};

    use super::*;
    use crate::jobs::fixtures::{job_from, params};

    fn selectors(dataset: &[&str], schema: &[&str], symbol: &[&str], job_id: &[&str]) -> Selectors {
        let own = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        Selectors {
            dataset: compile_all("--dataset", &own(dataset)).expect("valid"),
            schema: compile_all("--schema", &own(schema)).expect("valid"),
            symbol: compile_all("--symbol", &own(symbol)).expect("valid"),
            job_id: compile_all("--job-id", &own(job_id)).expect("valid"),
        }
    }

    #[test]
    fn no_selectors_accept_everything() {
        let job = job_from(&params(|_| {}));
        let empty = Selectors::default();
        assert!(!empty.needs_details());
        assert!(empty.accepts_id("anything"));
        assert!(empty.accepts(&job));
    }

    #[test]
    fn patterns_are_unanchored_and_case_insensitive() {
        let job = job_from(&params(|_| {}));
        assert!(selectors(&["glbx"], &[], &[], &[]).accepts(&job));
        assert!(selectors(&["GLBX"], &[], &[], &[]).accepts(&job));
        assert!(selectors(&["^GLBX\\.MDP3$"], &[], &[], &[]).accepts(&job));
        assert!(!selectors(&["^MDP3$"], &[], &[], &[]).accepts(&job));
    }

    /// Repeating a flag widens the selection; combining flags narrows it.
    #[test]
    fn repeated_flags_are_or_and_different_flags_are_and() {
        let job = job_from(&params(|_| {}));

        assert!(selectors(&["XNAS", "GLBX"], &[], &[], &[]).accepts(&job));
        assert!(!selectors(&["XNAS", "IFEU"], &[], &[], &[]).accepts(&job));

        // Right dataset, wrong schema: the AND has to fail.
        assert!(!selectors(&["GLBX"], &["ohlcv"], &[], &[]).accepts(&job));
        assert!(selectors(&["GLBX"], &["trades"], &[], &[]).accepts(&job));
    }

    /// The vendor echoes a multi-symbol selection as one comma-joined string, so a
    /// filter that matched against the joined form could match ACROSS the boundary
    /// between two symbols, or match the comma itself. Neither symbol contains what was
    /// asked for.
    #[test]
    fn symbols_are_matched_one_at_a_time_not_as_a_joined_string() {
        let job = job_from(&params(|p| {
            p.symbols = Symbols::Symbols(vec!["ES.FUT,MES.FUT".to_owned()]);
        }));

        assert!(selectors(&[], &[], &["^ES\\.FUT$"], &[]).accepts(&job));
        assert!(selectors(&[], &[], &["^MES\\.FUT$"], &[]).accepts(&job));
        // Spans the join. No symbol in this job is `T,M`.
        assert!(!selectors(&[], &[], &["T,M"], &[]).accepts(&job));
        assert!(!selectors(&[], &[], &[","], &[]).accepts(&job));
    }

    /// `--symbol A --symbol B` selects jobs holding EITHER, not jobs holding both.
    #[test]
    fn repeated_symbol_flags_do_not_require_every_symbol() {
        let job = job_from(&params(|p| {
            p.symbols = Symbols::Symbols(vec!["ES.FUT".to_owned()]);
        }));
        assert!(selectors(&[], &[], &["ES\\.FUT", "NQ\\.FUT"], &[]).accepts(&job));
    }

    #[test]
    fn a_whole_dataset_job_is_selectable_by_its_sentinel() {
        let job = job_from(&params(|p| p.symbols = Symbols::All));
        assert!(selectors(&[], &[], &["ALL_SYMBOLS"], &[]).accepts(&job));
        assert!(!selectors(&[], &[], &["^ES\\.FUT$"], &[]).accepts(&job));
    }

    /// `--job-id` needs only the short listing, which is what lets it prune before a
    /// single detail request is spent.
    #[test]
    fn a_job_id_selector_does_not_require_details() {
        let only_id = selectors(&[], &[], &[], &["GLBX-2026"]);
        assert!(!only_id.needs_details());
        assert!(only_id.accepts_id("GLBX-20260813-TESTJOB"));
        assert!(!only_id.accepts_id("XNAS-20260813-TESTJOB"));

        assert!(selectors(&[], &[], &["ES"], &[]).needs_details());
    }

    #[test]
    fn a_schema_selector_matches_the_written_schema() {
        let job = job_from(&params(|p| p.schema = Schema::Ohlcv1M));
        assert!(selectors(&[], &["ohlcv-1m"], &[], &[]).accepts(&job));
        assert!(selectors(&[], &["^ohlcv"], &[], &[]).accepts(&job));
        assert!(!selectors(&[], &["^trades$"], &[], &[]).accepts(&job));
    }

    #[test]
    fn an_invalid_pattern_is_reported_against_its_flag() {
        let err = compile_all("--symbol", &["ES(".to_owned()])
            .expect_err("an unclosed group is not a regex")
            .to_string();
        assert!(err.contains("--symbol"), "{err}");
        assert!(err.contains("ES("), "{err}");
    }
}
