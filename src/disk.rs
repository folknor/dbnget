//! Free-space checks.
//!
//! A month of MBP-1 is tens of gigabytes. Running the filesystem out of space part way
//! through leaves a partial file behind and takes the rest of the machine with it, so
//! the check happens before each download rather than after the damage.

use std::path::Path;

use anyhow::{Context, Result, bail};

const BYTES_PER_GIB: u64 = 1 << 30;

/// Bytes available to an unprivileged process on the filesystem holding `path`.
///
/// This is the available count, not the free count: the difference is the reserve only
/// root can use, and writing into it is not an option here.
pub fn available_bytes(path: &Path) -> Result<u64> {
    let stat = rustix::fs::statvfs(path)
        .with_context(|| format!("checking free space on {}", path.display()))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Refuses to continue when free space has fallen below the floor.
///
/// A floor of zero disables the check.
pub fn check_floor(path: &Path, floor_gib: u64) -> Result<()> {
    if floor_gib == 0 {
        return Ok(());
    }
    let available = available_bytes(path)?;
    let floor = floor_gib.saturating_mul(BYTES_PER_GIB);
    if available < floor {
        bail!(
            "only {:.1} GiB free on {}, below the --min-free-gb floor of {floor_gib} GiB",
            as_gib(available),
            path.display(),
        );
    }
    Ok(())
}

/// Refuses to start a download that would land the filesystem below the floor.
///
/// The floor alone is not enough. A job is many files downloaded in sequence: checking
/// only `available >= floor` lets a run start each file legally and still finish below
/// the floor, because nothing accounts for the bytes about to be written. The incoming
/// size has to be part of the sum, which is why this takes it.
///
/// A floor of zero disables the check entirely, incoming size included: the flag's
/// documented meaning is "do not police free space".
pub fn check_room_for(path: &Path, floor_gib: u64, incoming: u64) -> Result<()> {
    if floor_gib == 0 {
        return Ok(());
    }
    let available = available_bytes(path)?;
    let floor = floor_gib.saturating_mul(BYTES_PER_GIB);
    let needed = floor.saturating_add(incoming);
    if available < needed {
        bail!(
            "{:.1} GiB free on {}, but this file needs {:.1} GiB and --min-free-gb reserves {floor_gib} GiB on top",
            as_gib(available),
            path.display(),
            as_gib(incoming),
        );
    }
    Ok(())
}

/// Bytes as gibibytes, for reporting only.
fn as_gib(bytes: u64) -> f64 {
    // Precision loss here is irrelevant: the result is only ever printed to one decimal.
    #[expect(
        clippy::cast_precision_loss,
        reason = "the value is formatted for humans, not compared"
    )]
    let gib = bytes as f64 / BYTES_PER_GIB as f64;
    gib
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_floor_never_refuses() {
        check_floor(Path::new("."), 0).unwrap();
    }

    #[test]
    fn an_unreachable_floor_refuses() {
        // No filesystem has an exbibyte free.
        assert!(check_floor(Path::new("."), 1 << 30).is_err());
    }

    #[test]
    fn the_current_directory_has_some_space() {
        assert!(available_bytes(Path::new(".")).unwrap() > 0);
    }

    #[test]
    fn a_zero_floor_never_refuses_any_size() {
        check_room_for(Path::new("."), 0, u64::MAX).unwrap();
    }

    #[test]
    fn a_file_larger_than_the_disk_is_refused_before_it_starts() {
        assert!(check_room_for(Path::new("."), 1, u64::MAX - (1 << 30)).is_err());
    }

    #[test]
    fn a_reachable_floor_admits_a_small_file() {
        check_room_for(Path::new("."), 0, 1024).unwrap();
    }

    /// The whole point of the incoming size: a check that passes on the floor alone
    /// must still refuse when the file about to be written would eat through it.
    #[test]
    fn a_file_that_would_breach_the_floor_is_refused() {
        let available = available_bytes(Path::new(".")).unwrap();
        check_floor(Path::new("."), 1).unwrap();
        assert!(check_room_for(Path::new("."), 1, available).is_err());
    }
}
