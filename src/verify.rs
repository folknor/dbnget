//! Integrity checks for downloaded batch files.
//!
//! The client verifies checksums itself, but on a mismatch it only logs a warning and
//! returns success, and it skips a file whose size already matches without re-reading
//! it. Neither is enough to build on: a corrupt file stays on disk, reports as
//! downloaded, and is trusted by every later run. These checks are the ones whose
//! failure has to stop the program.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use databento::historical::batch::BatchFileDesc;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tracing::info;

/// The zstd frame magic number, little-endian.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Reading a whole file to hash it is IO-bound; a large buffer keeps the syscall count
/// down on the multi-gigabyte files a month of MBP-1 produces.
const READ_BUFFER: usize = 1 << 23;

/// Names Windows refuses regardless of extension, as a device rather than a file.
const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Rejects a name that is anything other than a plain, portable file name.
///
/// These names come off the wire and are joined onto the output directory, so a
/// separator or a parent-directory component would write outside it. The rules are the
/// union across platforms, not the rules of whatever is running: this crate is not
/// declared Unix-only, and a name that is merely awkward on Linux can be unwritable on
/// Windows - a colon makes an alternate data stream, a trailing dot or space is
/// silently trimmed into a different name, and `CON` or `NUL` is a device.
///
/// Being stricter than the running platform costs nothing here. The names are vendor
/// job ids and generated data filenames, none of which have any business containing
/// these characters, so a refusal means something has gone wrong upstream.
pub fn checked_file_name(filename: &str) -> Result<&str> {
    let refuse =
        |why: &str| -> anyhow::Error { anyhow::anyhow!("refusing filename `{filename}`: {why}") };

    if filename.is_empty() {
        return Err(refuse("empty"));
    }
    if filename == "." || filename == ".." {
        return Err(refuse("a directory reference, not a file"));
    }
    if let Some(bad) = filename.chars().find(|c| {
        matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*') || c.is_control()
    }) {
        return Err(refuse(&format!("contains `{}`", bad.escape_default())));
    }
    if filename.ends_with('.') || filename.ends_with(' ') {
        return Err(refuse("ends with a dot or a space"));
    }

    // The stem is what Windows matches against, so `NUL.txt` is as refused as `NUL`.
    let stem = filename.split('.').next().unwrap_or(filename);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return Err(refuse("a reserved device name"));
    }

    // Belt and braces: whatever the string looked like, it has to be exactly one
    // ordinary path component to the platform actually running.
    let mut components = Path::new(filename).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(one)), None) if one == filename => Ok(filename),
        _ => Err(refuse("not a single plain path component")),
    }
}

/// Refuses a destination that already exists as a symbolic link.
///
/// Both the client's `File::create` and this module's own reads follow links, so a
/// pre-created link at a name the manifest is about to claim would redirect the write,
/// and the verification after it, outside the job directory. Checked before the write
/// rather than after, because after is too late.
///
/// A missing path is fine - that is the ordinary case.
pub fn no_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => bail!(
            "refusing to write through the symbolic link at {}",
            path.display()
        ),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("checking what sits at {}", path.display())),
    }
}

/// Verifies one delivered file: size, checksum, and - for a zstd file - that it starts
/// with a zstd frame.
///
/// Fails loudly rather than reporting at the end, so a corrupt download cannot be
/// mistaken for a complete one by whatever runs next.
pub async fn file(path: &Path, desc: &BatchFileDesc) -> Result<PathBuf> {
    verify_file(path, desc).await?;
    info!(path = %path.display(), "verified");
    Ok(path.to_path_buf())
}

/// Checks one file's size, checksum, and - for a zstd file - that it starts with a
/// zstd frame.
async fn verify_file(path: &Path, desc: &BatchFileDesc) -> Result<()> {
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("stat {}", path.display()))?;
    if meta.len() != desc.size {
        bail!(
            "{}: expected {} bytes, found {}",
            path.display(),
            desc.size,
            meta.len()
        );
    }

    let Some((algo, expected)) = desc.hash.split_once(':') else {
        bail!(
            "{}: malformed manifest hash `{}`",
            path.display(),
            desc.hash
        );
    };
    if algo != "sha256" {
        bail!("{}: unsupported hash algorithm `{algo}`", path.display());
    }

    // Compared case-insensitively: hex is hex whichever case the vendor sends it in,
    // and treating an uppercase digest as a mismatch would condemn a perfectly good
    // file to be deleted and re-fetched on every run, forever.
    let actual = sha256_file(path).await?;
    if !actual.eq_ignore_ascii_case(expected) {
        bail!(
            "{}: checksum mismatch (manifest {expected}, on disk {actual})",
            path.display()
        );
    }

    if path.extension().is_some_and(|ext| ext == "zst") {
        check_zstd_frame(path).await?;
    }
    Ok(())
}

/// Streams a file through SHA-256 and returns the lowercase hex digest.
async fn sha256_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        let read = file
            .read(&mut buf)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Confirms the file opens with a zstd frame.
///
/// A checksum only proves the bytes arrived as the vendor recorded them. This catches
/// the other case: the vendor recorded, and faithfully delivered, something that is not
/// the compressed data it claims to be. Only the magic number is checked - decoding
/// further would mean materializing market data to prove a point.
async fn check_zstd_frame(path: &Path) -> Result<()> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .await
        .with_context(|| format!("{}: too short to hold a zstd frame", path.display()))?;
    if magic != ZSTD_MAGIC {
        bail!(
            "{}: does not start with a zstd frame (found {})",
            path.display(),
            hex(&magic)
        );
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: [char; 16] = [
        '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
    ];

    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)]);
        out.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    out
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "assertions in tests should panic loudly"
)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_are_accepted() {
        assert_eq!(
            checked_file_name("glbx-mdp3-20240501.trades.csv.zst").unwrap(),
            "glbx-mdp3-20240501.trades.csv.zst"
        );
    }

    #[test]
    fn escaping_names_are_refused() {
        for bad in ["", ".", "..", "../evil", "a/b", "a\\b", "/etc/passwd"] {
            assert!(
                checked_file_name(bad).is_err(),
                "should have refused `{bad}`"
            );
        }
    }

    /// Refused everywhere, not just where they happen to be illegal. These names are
    /// writable on Linux and are not on Windows, and the crate does not claim to be
    /// Unix-only.
    #[test]
    fn names_windows_cannot_write_are_refused() {
        for bad in [
            "C:evil",
            "stream:name",
            "trailing.",
            "trailing ",
            "CON",
            "nul.txt",
            "COM1",
            "what?",
            "star*",
            "pipe|name",
            "quote\"name",
            "less<than",
            "bell\u{7}",
        ] {
            assert!(
                checked_file_name(bad).is_err(),
                "should have refused `{bad}`"
            );
        }
    }

    #[test]
    fn ordinary_vendor_names_still_pass() {
        for good in [
            "glbx-mdp3-20240501.trades.csv.zst",
            "GLBX-20260813-K8LCTCLNJJ",
            "manifest.json",
            "condition.json",
            ".hidden",
        ] {
            assert!(
                checked_file_name(good).is_ok(),
                "should have accepted `{good}`"
            );
        }
    }

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
