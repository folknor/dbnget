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
}
