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

/// Rejects a manifest entry whose filename is anything other than a plain file name.
///
/// The name is joined onto the output directory, so a name containing a separator or a
/// parent-directory component would write outside it.
pub fn checked_file_name(filename: &str) -> Result<&str> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename == "."
        || filename == ".."
    {
        bail!("refusing manifest filename `{filename}`: not a plain file name");
    }
    Ok(filename)
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

    let actual = sha256_file(path).await?;
    if actual != expected {
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

    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
