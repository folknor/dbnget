//! An untrusted index from job id to the request that job was submitted with.
//!
//! # What this is not
//!
//! It is NOT a ledger. Nothing here is authoritative about what the account bought, and
//! no value read from this store can decide whether dbnget spends money. The vendor's
//! job listing remains the only account of what was actually purchased.
//!
//! The distinction is what makes a persistent store admissible at all, and it is
//! enforced by the shape of the API rather than by convention. This module hands out
//! [`RequestKey`] values that say "job X was submitted with request R, probably". A
//! caller can use that to decide WHERE TO LOOK FIRST and for nothing else:
//!
//! - Adoption requires live confirmation. A proposed candidate is fetched with
//!   `get_job_details` and re-matched against the live response before anything is
//!   downloaded, so a false entry costs one wasted request.
//! - Submission requires live exhaustion. A run that is about to charge sweeps every
//!   job in the live listing first, so a missing or wrong entry cannot cause a
//!   duplicate purchase.
//!
//! Those two rules are the whole safety argument, and they live in `jobs::reconcile`.
//! If a future caller reads a value from here and acts on it without live
//! confirmation, this module has been misused and the double-charge risk is back.
//!
//! # Why identity only
//!
//! An entry holds only the fields frozen when the vendor accepts a submission - the
//! match key, plus `id` and `ts_received` to join against the live listing. It
//! deliberately holds no `state`, no `record_count`, no sizes, costs or processing
//! timestamps. Those move over a job's life, and storing them would create a stale
//! surface that buys nothing: the live listing already supplies state, and the
//! confirming detail response supplies the rest.
//!
//! Holding identity only is also why entries are written for jobs in EVERY state. An
//! earlier design cached finished jobs only, because a queued job's record count and
//! sizes are not settled yet. That exclusion was an artifact of mixing identity with
//! results. A queued job's REQUEST is fixed the moment it is accepted, so the poll
//! loop, which is the hot path here - the same command re-run until a job is ready -
//! gets to propose a candidate on the first re-run instead of sweeping the account
//! every time.
//!
//! # Failure is never a reconciliation result
//!
//! Every failure in this module disables the cache for the run and leaves the live
//! algorithm untouched: a missing directory, a hostile permission mode, a corrupt file,
//! an unwritable disk. None of them may turn into "no matching job", because that
//! sentence is what authorises a charge. [`Cache::disabled`] is a first-class state and
//! the whole program works without ever reading a byte from here.

use std::{
    fs,
    path::{Path, PathBuf},
};

use databento::historical::batch::{BatchJob, SubmitJobParams};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::debug;

use crate::jobs;

/// Bumped when the stored shape changes. It is a path component, so a bump orphans
/// every older entry rather than risking a stale one being read under new rules.
const SCHEMA_VERSION: &str = "v1";

/// Refuses to read an entry larger than this. A hint file is a few hundred bytes; a
/// large one is a corrupt or hostile file and reading it is an availability problem.
const MAX_ENTRY_BYTES: u64 = 64 * 1024;

/// An upper bound on the symbols an entry may claim, so a hostile file cannot make the
/// matcher do unbounded work. Far above any real request.
const MAX_SYMBOLS: usize = 20_000;

/// The request identity of a job: exactly the fields [`jobs::job_matches`] compares,
/// in a form that can be persisted and compared without the vendor's types.
///
/// # Under-specifying this is safe, and that is deliberate
///
/// If a field that matching reads is missing here, two jobs differing only in that
/// field become indistinguishable to the index, so it proposes a candidate that live
/// confirmation then rejects. The cost is one wasted `get_job_details`.
///
/// That is the OPPOSITE of how [`jobs::job_matches`] fails. A field missing there is a
/// silent double charge, which is why the test fixtures force a new upstream field to
/// break the build. This type needs no such guard, because every direction it can be
/// wrong in is a performance bug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestKey {
    dataset: String,
    schema: String,
    /// Canonical form: sorted, uppercased, deduplicated, commas split. Shares
    /// [`jobs::canonical_symbols`] with the matcher so the two cannot drift.
    symbols: Vec<String>,
    stype_in: String,
    stype_out: String,
    encoding: String,
    compression: String,
    split_duration: String,
    split_size: Option<u64>,
    split_symbols: bool,
    delivery: String,
    pretty_px: bool,
    pretty_ts: bool,
    map_symbols: bool,
    limit: Option<u64>,
    /// Nanosecond instants, matching how [`jobs::job_matches`] compares bounds.
    start_ns: i128,
    end_ns: i128,
}

impl RequestKey {
    /// Builds a key from the vendor's echo of a job.
    ///
    /// This is the ONLY way an entry is created, and the reason is normalization. The
    /// vendor applies its own defaults and spellings to a submission - the
    /// encoding-dependent `map_symbols` default, symbol case and ordering, comma-joined
    /// selections, the `ALL_SYMBOLS` representation - and the matcher compares against
    /// that echoed shape. Synthesizing a key from `SubmitJobParams` would index the
    /// SENT shape instead, so lookups would miss, and worse, a canonicalization that
    /// collapses two sent forms could propose the wrong job. Both are survivable
    /// because confirmation is live, but neither is worth reasoning about when the echo
    /// is always available - `submit_job` returns one too.
    pub fn from_batch_job(job: &BatchJob) -> Self {
        Self {
            dataset: job.dataset.clone(),
            schema: job.schema.to_string(),
            symbols: jobs::canonical_symbols(&job.symbols),
            stype_in: job.stype_in.to_string(),
            stype_out: job.stype_out.to_string(),
            encoding: job.encoding.to_string(),
            compression: job.compression.to_string(),
            split_duration: job.split_duration.to_string(),
            split_size: job.split_size.map(std::num::NonZeroU64::get),
            split_symbols: job.split_symbols,
            delivery: job.delivery.to_string(),
            pretty_px: job.pretty_px,
            pretty_ts: job.pretty_ts,
            map_symbols: job.map_symbols,
            limit: job.limit.map(std::num::NonZeroU64::get),
            start_ns: job.start.unix_timestamp_nanos(),
            end_ns: job.end.unix_timestamp_nanos(),
        }
    }

    /// Builds the key to LOOK UP a request with. Never used to store one.
    ///
    /// The asymmetry is deliberate and is the whole reason both constructors exist.
    /// Entries are written from the vendor's echo so they carry the vendor's
    /// normalization; a lookup starts from what the user asked for and has to arrive at
    /// the same shape. The two places that shape differs are handled here and nowhere
    /// else: `map_symbols` is an `Option<bool>` on a submission against a concrete
    /// `bool` on a job, with an encoding-dependent default, and symbol spelling is
    /// folded by the canonicalization both sides share.
    ///
    /// If these ever drift, lookups miss and every run takes the sweep. That is slow
    /// and not wrong, which is the correct direction for this type to fail.
    pub fn from_submit_params(params: &SubmitJobParams) -> Self {
        Self {
            dataset: params.dataset.clone(),
            schema: params.schema.to_string(),
            symbols: jobs::canonical_symbols(&params.symbols),
            stype_in: params.stype_in.to_string(),
            stype_out: params.stype_out.to_string(),
            encoding: params.encoding.to_string(),
            compression: params.compression.to_string(),
            split_duration: params.split_duration.to_string(),
            split_size: params.split_size.map(std::num::NonZeroU64::get),
            split_symbols: params.split_symbols,
            delivery: params.delivery.to_string(),
            pretty_px: params.pretty_px,
            pretty_ts: params.pretty_ts,
            map_symbols: jobs::effective_map_symbols(params),
            limit: params.limit.map(std::num::NonZeroU64::get),
            start_ns: params.date_time_range.start.unix_timestamp_nanos(),
            end_ns: params.date_time_range.end.unix_timestamp_nanos(),
        }
    }
}

/// One stored hint: which job, received when, and what it was asked for.
///
/// `ts_received` is stored so the entry can be joined against the live short listing.
/// An entry whose received-time disagrees with the live record is discarded rather than
/// used, which is what stops a recycled or misassociated id from proposing a candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    #[serde(with = "time::serde::rfc3339")]
    ts_received: OffsetDateTime,
    key: RequestKey,
}

/// The on-disk request index, or a disabled stand-in that answers nothing.
#[derive(Debug)]
pub struct Cache {
    /// `None` disables every operation. Reached whenever the store cannot be used
    /// safely, which is a performance outcome and never an error the caller sees.
    dir: Option<PathBuf>,
}

impl Cache {
    /// A cache that never proposes anything and never stores anything.
    ///
    /// Every failure path lands here, and so does every platform where a private
    /// directory cannot be established.
    pub const fn disabled() -> Self {
        Self { dir: None }
    }

    /// Opens the index for the account identified by `api_key`, or returns a disabled
    /// cache if it cannot be made private.
    ///
    /// Never fails. A cache problem must not be able to take down a command that would
    /// otherwise work, because the live algorithm does not need this store at all.
    ///
    /// Partitioning is by a digest of the credential rather than by account identity,
    /// which is imprecise in one harmless direction: two keys on one account keep two
    /// cold indexes. Since nothing here decides anything, redundancy costs requests and
    /// not correctness. The raw key never appears in a path, a file, or a log.
    pub fn open(api_key: &str) -> Self {
        match Self::try_open(api_key) {
            Ok(dir) => Self { dir: Some(dir) },
            Err(err) => {
                debug!(%err, "request index unavailable; continuing without it");
                Self::disabled()
            }
        }
    }

    fn try_open(api_key: &str) -> anyhow::Result<PathBuf> {
        let mut dir = dirs::cache_dir().ok_or_else(|| anyhow::anyhow!("no cache directory"))?;
        // Every level dbnget owns is made private, not just the leaf holding the
        // entries. The account slug is a directory NAME, so a world-listable parent
        // discloses it to any other user on the machine even when its contents are
        // sealed - and that name is a digest of the credential.
        for component in ["dbnget", SCHEMA_VERSION, &account_slug(api_key)] {
            dir.push(component);
            create_private_dir(&dir)?;
        }
        Ok(dir)
    }

    /// The request identity recorded for `id`, if one is stored and agrees with the
    /// live listing's received-time.
    ///
    /// A `None` here means "this store has nothing useful to say", which is the only
    /// thing it is ever allowed to mean. It never means "no such job" and it never
    /// means "that job does not match".
    pub fn get(&self, id: &str, live_ts_received: OffsetDateTime) -> Option<RequestKey> {
        let entry = self.read_entry(id)?;
        // Joining on both fields is what keeps a stale or misassociated file from
        // proposing a candidate for a job it never described.
        if entry.id != id || entry.ts_received != live_ts_received {
            debug!(
                job_id = id,
                "index entry disagrees with the live listing; ignoring"
            );
            return None;
        }
        if entry.key.symbols.len() > MAX_SYMBOLS {
            debug!(
                job_id = id,
                "index entry claims implausibly many symbols; ignoring"
            );
            return None;
        }
        Some(entry.key)
    }

    fn read_entry(&self, id: &str) -> Option<Entry> {
        let path = self.entry_path(id)?;
        // A file too large to be a hint is not read at all, so a hostile or corrupt
        // entry cannot make this allocate against its own declared size.
        match fs::metadata(&path) {
            Ok(meta) if meta.len() > MAX_ENTRY_BYTES => {
                debug!(job_id = id, "index entry is implausibly large; ignoring");
                return None;
            }
            Ok(meta) if !meta.is_file() => return None,
            Ok(_) => {}
            Err(_) => return None,
        }
        let bytes = fs::read(&path).ok()?;
        // A corrupt entry is a miss, never an error. Anything else would let a bad byte
        // on disk decide a reconciliation.
        serde_json::from_slice(&bytes).ok()
    }

    /// Records the request identity the vendor echoed for `job`.
    ///
    /// Best effort by construction. A write that fails is logged at debug and otherwise
    /// ignored, because the caller is either mid-reconciliation or has just submitted a
    /// job that may already have charged - and in the second case an error here would
    /// read as a failed submission, which is the one impression that must not be given.
    pub fn put(&self, job: &BatchJob) {
        if let Err(err) = self.try_put(job) {
            debug!(job_id = %job.id, %err, "could not record the job in the request index");
        }
    }

    fn try_put(&self, job: &BatchJob) -> anyhow::Result<()> {
        let Some(path) = self.entry_path(&job.id) else {
            return Ok(());
        };
        let entry = Entry {
            id: job.id.clone(),
            ts_received: job.ts_received,
            key: RequestKey::from_batch_job(job),
        };
        let bytes = serde_json::to_vec(&entry)?;
        write_private_atomic(&path, &bytes)
    }

    /// The path an entry lives at, or `None` when the cache is disabled or the id is
    /// not usable as a file name.
    ///
    /// Job ids are vendor-supplied and get the same validation as manifest filenames
    /// before being joined onto a directory, because that is exactly what they are
    /// here: untrusted input becoming a path component.
    fn entry_path(&self, id: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let name = crate::verify::checked_file_name(id).ok()?;
        Some(dir.join(format!("{name}.json")))
    }
}

/// A stable, non-reversible directory name for a credential.
///
/// Domain-separated with a fixed prefix so the digest cannot collide with a hash of the
/// same key computed for some other purpose, and truncated to a length where a
/// collision is not a realistic concern. The key itself is never written anywhere.
fn account_slug(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"dbnget-request-index-v1\0");
    hasher.update(api_key.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

/// Creates `dir` and every parent, then refuses it unless it is a real directory that
/// only this user can reach.
///
/// The contents are account activity - datasets, symbols, date ranges - so the trust
/// boundary is a directory nobody else can read or write. Once that holds, no other
/// user can introduce a symlink or replace an entry inside it, which is what makes the
/// per-entry handling below sufficient.
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    // `create_dir_all` succeeds on an existing symlink to a directory, so the check has
    // to be against the un-followed metadata.
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    enforce_private(dir, &meta)
}

/// Refuses a directory any other user can reach, tightening it if it is merely loose.
///
/// Ownership is checked implicitly rather than by comparing uids, which would mean
/// either a `libc` dependency or an `unsafe` call for `geteuid`. A directory this user
/// does not own fails here anyway: if its mode is loose, `set_permissions` returns
/// `EPERM`, and if it is already private then this user cannot read or write inside it,
/// so every later operation misses. Both land on a disabled cache, which is the correct
/// outcome for a store nothing depends on.
#[cfg(unix)]
fn enforce_private(dir: &Path, meta: &fs::Metadata) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    // Tighten rather than trust the umask: a directory left group- or world-accessible
    // by an earlier run, or by the user, would expose account activity.
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// On a platform where a private directory cannot be verified, the index is disabled
/// rather than written into a location whose access this code cannot reason about.
/// Correctness is unaffected: every command works with the cache off.
#[cfg(not(unix))]
fn enforce_private(dir: &Path, _meta: &fs::Metadata) -> anyhow::Result<()> {
    anyhow::bail!(
        "{} cannot be verified as user-private on this platform",
        dir.display()
    )
}

/// Writes `bytes` to `path` so that a reader sees either the old file or the whole new
/// one, never a partial write.
///
/// The temporary lands in the same directory as its destination, because `rename` is
/// only atomic within a filesystem. Its name carries the process id so two concurrent
/// runs - the documented fan-out workflow - cannot collide on it. One file per job id
/// means concurrent writers touch disjoint paths, so there is no lost-update problem to
/// solve: nothing here merges.
fn write_private_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("entry path has no parent"))?;
    let temp = dir.join(format!(".tmp-{}-{}", std::process::id(), file_stem(path)));

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    set_private_mode(&mut options);
    let mut file = options.open(&temp)?;

    let written = std::io::Write::write_all(&mut file, bytes)
        .and_then(|()| std::io::Write::flush(&mut file))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err()
        && let Err(err) = fs::remove_file(&temp)
    {
        // Leaving a temporary behind on a failed write would accumulate one per attempt.
        debug!(%err, "could not clean up a temporary index file");
    }
    Ok(written?)
}

fn file_stem(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "entry".to_owned(), |n| n.to_string_lossy().into_owned())
}

#[cfg(unix)]
fn set_private_mode(options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn set_private_mode(_options: &mut fs::OpenOptions) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::fixtures;

    #[test]
    fn a_disabled_cache_answers_nothing_and_stores_nothing() {
        let cache = Cache::disabled();
        let job = fixtures::job_from(&fixtures::params(|_| {}));
        // Both directions have to be inert: a disabled cache that errored on `put`
        // would turn a cache problem into a command failure.
        cache.put(&job);
        assert!(cache.get(&job.id, job.ts_received).is_none());
    }

    #[test]
    fn a_key_round_trips_through_the_stored_form() {
        let job = fixtures::job_from(&fixtures::params(|_| {}));
        let entry = Entry {
            id: job.id.clone(),
            ts_received: job.ts_received,
            key: RequestKey::from_batch_job(&job),
        };
        let bytes = serde_json::to_vec(&entry).expect("entry serializes");
        let back: Entry = serde_json::from_slice(&bytes).expect("entry deserializes");
        assert_eq!(back.key, RequestKey::from_batch_job(&job));
        assert_eq!(back.ts_received, job.ts_received);
    }

    #[test]
    fn the_key_separates_requests_the_matcher_separates() {
        let base = fixtures::job_from(&fixtures::params(|_| {}));
        let key = RequestKey::from_batch_job(&base);

        // Every field the matcher reads must move the key, or the index would propose a
        // candidate for a request it cannot deliver. That is survivable - confirmation
        // is live - but it is wasted work on the hot path.
        let mut other = base.clone();
        other.schema = databento::dbn::Schema::Mbo;
        assert_ne!(key, RequestKey::from_batch_job(&other));

        let mut other = base.clone();
        other.encoding = databento::dbn::Encoding::Csv;
        assert_ne!(key, RequestKey::from_batch_job(&other));

        let mut other = base.clone();
        other.end = base.end + time::Duration::nanoseconds(1);
        assert_ne!(key, RequestKey::from_batch_job(&other));

        let mut other = base.clone();
        other.symbols = databento::Symbols::Symbols(vec!["NQM4".to_owned()]);
        assert_ne!(key, RequestKey::from_batch_job(&other));
    }

    #[test]
    fn symbol_spelling_does_not_change_the_key() {
        // The key shares canonicalization with the matcher, so the forms the vendor is
        // free to echo interchangeably have to land on one entry.
        let mut lower = fixtures::job_from(&fixtures::params(|_| {}));
        lower.symbols = databento::Symbols::Symbols(vec!["esm4".to_owned(), "NQM4".to_owned()]);
        let mut joined = fixtures::job_from(&fixtures::params(|_| {}));
        joined.symbols = databento::Symbols::Symbols(vec!["NQM4,ESM4".to_owned()]);
        assert_eq!(
            RequestKey::from_batch_job(&lower),
            RequestKey::from_batch_job(&joined)
        );
    }

    #[test]
    fn the_lookup_key_equals_the_stored_key_for_the_same_request() {
        // THE invariant of this module. Entries are written from the vendor's echo and
        // looked up from the user's submission, and if those two constructors ever
        // disagree the index proposes nothing, every run takes the exhaustive sweep,
        // and nothing fails - it just quietly gets slow. Nothing else would catch that.
        for edit in [
            (|_: &mut _| {}) as fn(&mut databento::historical::batch::SubmitJobParams),
            |p| p.encoding = databento::dbn::Encoding::Csv,
            |p| p.encoding = databento::dbn::Encoding::Json,
            |p| p.map_symbols = Some(true),
            |p| p.map_symbols = Some(false),
            |p| p.symbols = databento::Symbols::All,
            |p| p.limit = std::num::NonZeroU64::new(10),
            |p| p.stype_out = databento::dbn::SType::RawSymbol,
        ] {
            let params = fixtures::params(edit);
            let job = fixtures::job_from(&params);
            assert_eq!(
                RequestKey::from_submit_params(&params),
                RequestKey::from_batch_job(&job),
                "lookup and stored keys diverged, so the index would never propose",
            );
        }
    }

    #[test]
    fn the_encoding_dependent_map_symbols_default_does_not_split_the_key() {
        // The trap this module inherits from the matcher: a submission carries
        // `Option<bool>` and a job echoes a concrete `bool`, defaulted by encoding. A
        // lookup that skipped that resolution would miss on every text-encoded request.
        let params = fixtures::params(|p| {
            p.encoding = databento::dbn::Encoding::Csv;
            p.map_symbols = None;
        });
        let key = RequestKey::from_submit_params(&params);
        assert!(key.map_symbols, "CSV defaults to a symbol column");
        assert_eq!(
            key,
            RequestKey::from_batch_job(&fixtures::job_from(&params))
        );
    }

    #[test]
    fn the_account_slug_hides_the_key_and_separates_credentials() {
        let one = account_slug("db-aaaaaaaaaaaaaaaaaaaa");
        let two = account_slug("db-bbbbbbbbbbbbbbbbbbbb");
        assert_ne!(one, two);
        assert_eq!(one.len(), 32);
        assert!(
            !one.contains("aaaa"),
            "the credential must not survive in the path"
        );
        assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
