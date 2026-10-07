//! Streaming SHA-1 file hashing — port of gotohp `core/sha1calc.go`.
//!
//! SHA-1 here is a *content identifier demanded by the Google Photos
//! protocol* (dedupe lookup + the `X-Goog-Hash` header), not a security
//! control. Do not "upgrade" it to SHA-256: the server contract is SHA-1.
//!
//! Go semantics preserved exactly:
//! - 1 MiB read buffer (`copyBufferSize` in sha1calc.go).
//! - The cancellation context is only consulted after every 64 MiB
//!   (`contextCheckInterval`): Go checks at the start of a chunked write once
//!   >= 64 MiB have accumulated since the last check. A consequence kept on
//!   purpose: cancelling the hash of a file smaller than 64 MiB has no effect
//!   until the threshold is crossed, exactly as upstream.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha1::{Digest, Sha1};

use crate::types::CancellationToken;
use crate::{Error, Result};

/// Check the cancellation token every 64 MiB (Go `contextCheckInterval`).
const CONTEXT_CHECK_INTERVAL: u64 = 64 * 1024 * 1024;
/// 1 MiB copy buffer (Go `copyBufferSize`).
const COPY_BUFFER_SIZE: usize = 1024 * 1024;

/// Compute the SHA-1 digest of the file at `path`, streaming it in 1 MiB
/// chunks. Mirrors Go `CalculateSHA1(ctx, filePath)`.
pub fn compute_sha1(path: &Path, cancel: &CancellationToken) -> Result<[u8; 20]> {
    let mut file =
        File::open(path).map_err(|e| Error::Other(format!("error opening file: {e}")))?;

    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; COPY_BUFFER_SIZE];
    let mut bytes_since_check: u64 = 0;

    loop {
        // Go's chunkedContextWriter checks before a write once the interval
        // has accumulated; the loop-top check here is the same cadence.
        if bytes_since_check >= CONTEXT_CHECK_INTERVAL {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            bytes_since_check = 0;
        }

        let n = file
            .read(&mut buf)
            .map_err(|e| Error::Other(format!("error calculating hash: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_since_check += n as u64;
    }

    let digest = hasher.finalize();
    let mut out = [0u8; 20];
    out.copy_from_slice(&digest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("outh-sha1calc-test-{}-{name}", std::process::id()));
        let mut f = File::create(&path).expect("create temp file");
        f.write_all(contents).expect("write temp file");
        path
    }

    fn hex(digest: &[u8; 20]) -> String {
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn empty_file_matches_known_sha1() {
        let path = temp_file("empty", b"");
        let digest = compute_sha1(&path, &CancellationToken::new()).expect("hash");
        assert_eq!(hex(&digest), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn abc_matches_known_sha1() {
        let path = temp_file("abc", b"abc");
        let digest = compute_sha1(&path, &CancellationToken::new()).expect("hash");
        assert_eq!(hex(&digest), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn streams_across_buffer_boundary() {
        // 1 MiB + 3 bytes of a deterministic pattern: exercises the chunked
        // read loop past COPY_BUFFER_SIZE.
        let mut contents = vec![0u8; 1024 * 1024 + 3];
        for (i, b) in contents.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let path = temp_file("big", &contents);
        let digest = compute_sha1(&path, &CancellationToken::new()).expect("hash");

        let mut hasher = Sha1::new();
        hasher.update(&contents);
        let mut want = [0u8; 20];
        want.copy_from_slice(&hasher.finalize());
        assert_eq!(digest, want);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cancelled_token_aborts_after_check_interval() {
        // Go only consults the context once 64 MiB have accumulated, so the
        // fixture must be larger than the interval. Zeros hash fast.
        let path = temp_file("cancel", &vec![0u8; 65 * 1024 * 1024]);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = compute_sha1(&path, &cancel).expect_err("must cancel");
        assert!(matches!(err, Error::Cancelled), "got {err:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_error() {
        let path = std::env::temp_dir().join("outh-sha1calc-test-does-not-exist");
        let result = compute_sha1(&path, &CancellationToken::new());
        assert!(result.is_err());
    }
}
