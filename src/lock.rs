//! An exclusive claim on a job's output directory.
//!
//! The batch download path had no claim of any kind, while `--immediate` has had one
//! from the start. Two runs of the same command therefore both walked the manifest and
//! both wrote into `OUT/JOB_ID/`, and the vendor client writes straight to the final
//! path with no temporary file - so concurrent runs interleaved their writes into one
//! file, and the loser's bytes were whatever the winner had not overwritten.
//!
//! It matters more now that an incomplete file is kept for the client to resume rather
//! than deleted. "Shorter than the manifest says" has to mean "this run was interrupted"
//! and not "another run is downloading it right now", and only a lock can tell those
//! apart.
//!
//! The mechanism is an advisory lock on a file, which is what `flock` and Windows'
//! `LockFileEx` both provide, and the reason for choosing it over an exclusively-created
//! marker file is the release semantics: the operating system drops the lock when the
//! holding process exits, whether that is a normal return, a panic, or `SIGKILL`. There
//! is no staleness to detect and no recovery path to get wrong. A marker file would need
//! a recorded pid and a liveness check, and would go stale exactly here - on a
//! multi-gigabyte download, which is the operation most likely to be interrupted.

use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use anyhow::{Context, Result};
use fs4::{FileExt, TryLockError};

use crate::verify;

/// The lock file's name inside a job's output directory.
///
/// It sits beside the data rather than in a machine-wide location on purpose. The
/// resource being guarded is one output directory, so a global lock would serialize
/// every dbnget on the machine and break the documented workflow of submitting a wave of
/// requests and re-running until none exits 3. A temporary-directory path would be worse
/// than useless: `TMPDIR` varies per user and under sandboxing, so two users writing one
/// shared output directory could take "the same" lock and not exclude each other at all.
const LOCK_FILE: &str = ".dbnget-lock";

/// Whether a name from the vendor's manifest would collide with the lock file.
///
/// Manifest filenames are untrusted input joined onto the output directory, and this
/// module puts its own file in that same namespace. A job delivering a file by this name
/// would have it downloaded onto the lock: on Unix the data would silently double as
/// lock storage, and on Windows the range lock can fail the client's own writes. No real
/// job is expected to carry this name - which is the point, since it means refusing it
/// costs nothing and removes the question.
///
/// Compared case-insensitively, because the collision is decided by the filesystem
/// rather than by us, and Windows and macOS would treat `.DBNGET-LOCK` as the same file.
pub fn is_lock_file(name: &str) -> bool {
    name.eq_ignore_ascii_case(LOCK_FILE)
}

/// A held claim on an output directory. Releasing it is the operating system's job:
/// dropping this closes the file, and the process dying closes it too.
///
/// The open file is the entire contents, because the open file is the entire mechanism.
#[derive(Debug)]
#[must_use = "the claim is released the moment this is dropped"]
pub struct Claim {
    /// Never read, and that is not an oversight: holding the file open IS the lock, and
    /// closing it is what releases it.
    _file: File,
}

/// Claims `dir` exclusively, or reports that someone else holds it.
///
/// `Ok(None)` means another process is working in this directory right now. That is not
/// a failure: it is the same "not finished, come back" state a queued job is in, and the
/// caller maps it to the same exit code, so a polling loop handles it without special
/// cases.
///
/// Never blocks. Waiting is right for a build queue, where the wait is bounded by
/// somebody else's compile; here it would be bounded by somebody else's multi-gigabyte
/// download, and a run that appears to hang is worse than one that says what is going on
/// and lets the caller decide.
pub fn try_claim(dir: &Path) -> Result<Option<Claim>> {
    let path = dir.join(LOCK_FILE);

    // The lock file is opened, never created-exclusively, because it is expected to
    // outlive the run that made it - the lock is the kernel's, not the file's existence.
    // It is not truncated for the same reason: nothing reads its contents, and
    // truncating would be a write to a file another process is holding.
    verify::no_symlink(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening the lock file {}", path.display()))?;

    match FileExt::try_lock(&file) {
        Ok(()) => Ok(Some(Claim { _file: file })),
        // Contention, which is an answer rather than an error.
        Err(TryLockError::WouldBlock) => Ok(None),
        // Anything else is a genuine filesystem problem and must not be mistaken for a
        // busy directory: a lock that cannot be taken because the filesystem does not
        // support locking would otherwise look exactly like a concurrent download, and
        // every run would report "busy" forever.
        Err(TryLockError::Error(err)) => {
            Err(anyhow::Error::new(err)).with_context(|| format!("locking {}", path.display()))
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Each test gets its own directory under `target/`, since the whole point of the
    /// thing under test is that two callers on one directory interfere.
    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-locks")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Manifest names share a directory with the lock file, so the one name that must
    /// never be accepted from the vendor is this one - in any case the filesystem would
    /// fold together.
    #[test]
    fn the_lock_file_name_is_recognised_whatever_its_case() {
        assert!(is_lock_file(LOCK_FILE));
        assert!(is_lock_file(".DBNGET-LOCK"));
        assert!(is_lock_file(".DbnGet-Lock"));

        assert!(!is_lock_file("dbnget-lock"));
        assert!(!is_lock_file(".dbnget-lock.csv"));
        assert!(!is_lock_file("xnas-itch-20220610.ohlcv-1m.csv"));
    }

    #[test]
    fn a_free_directory_can_be_claimed() {
        let dir = scratch("free");
        let claim = try_claim(&dir).unwrap();
        assert!(claim.is_some());
        assert!(dir.join(LOCK_FILE).exists());
    }

    /// The case the whole module exists for. An advisory lock excludes per open file
    /// description, so a second claim contends even from within one process - which is
    /// what makes this testable without spawning anything.
    #[test]
    fn a_claimed_directory_cannot_be_claimed_again() {
        let dir = scratch("contended");
        let held = try_claim(&dir).unwrap();
        assert!(held.is_some(), "the first claim should succeed");
        assert!(
            try_claim(&dir).unwrap().is_none(),
            "a second claim on a held directory must report contention, not take it"
        );
    }

    /// Release is the operating system's, so dropping the guard frees the directory
    /// without any cleanup step that could be skipped or crash before it runs.
    #[test]
    fn dropping_a_claim_releases_it() {
        let dir = scratch("released");
        let held = try_claim(&dir).unwrap();
        assert!(held.is_some());
        drop(held);
        assert!(
            try_claim(&dir).unwrap().is_some(),
            "the directory should be free once the claim is dropped"
        );
    }

    /// The lock file outliving the run that made it is normal and must not be read as
    /// the directory still being held. This is the property a marker file would not
    /// have.
    #[test]
    fn a_leftover_lock_file_does_not_hold_the_directory() {
        let dir = scratch("leftover");
        drop(try_claim(&dir).unwrap().unwrap());
        assert!(
            dir.join(LOCK_FILE).exists(),
            "the file is deliberately left behind"
        );
        assert!(
            try_claim(&dir).unwrap().is_some(),
            "a stale lock file is not a held lock"
        );
    }
}
