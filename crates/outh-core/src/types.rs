//! Shared types for outh-core, per CONTRACT.md §Core public surface.
//!
//! These mirror gotohp's `core/reporter.go` event payloads (`ThreadStatus`,
//! `FileUploadResult`, `PreflightWarning`, `AlbumStatus`) in the simplified
//! shape frozen by the contract. The upload engine (upload.rs) produces them;
//! the app supplies an [`UploadReporter`] implementation that renders them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative cancellation flag shared between the UI and worker threads.
///
/// Port of the `context.Context` cancellation threaded through gotohp's
/// upload pipeline (core/upload.go, core/api.go `UploadFileWithProgress`).
/// Cheap to clone: every clone points at the same flag.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        CancellationToken {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-file pipeline stage. Mirrors the `Status` strings of Go's
/// `ThreadStatus` (core/upload.go): "hashing", "checking", "uploading",
/// "finalizing", "completed", "skipped", "error".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Hashing,
    Checking,
    Uploading,
    Finalizing,
    Completed,
    Skipped,
    Error,
}

/// Status of one worker thread for one file; emitted repeatedly as progress
/// changes. Port of Go's `ThreadStatus` (core/upload.go), contract shape.
#[derive(Debug, Clone)]
pub struct ThreadStatus {
    pub worker: usize,
    pub stage: Stage,
    pub file_path: PathBuf,
    pub bytes_uploaded: u64,
    pub bytes_total: u64,
    /// 1-based attempt number; 0 when not applicable (Go: `Attempt`).
    pub attempt: u32,
}

/// Terminal outcome of one file in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Uploaded,
    /// HashCheck found the file already in the library; zero bytes uploaded.
    SkippedAlreadyPresent,
    SkippedUnsupported,
    Failed,
}

/// Final result for one file. Port of Go's `FileUploadResult`
/// (core/upload.go), contract shape.
#[derive(Debug, Clone)]
pub struct FileResult {
    pub file_path: PathBuf,
    pub outcome: Outcome,
    pub media_key: Option<String>,
    pub message: Option<String>,
}

/// A non-fatal preflight finding (e.g. a Live Photo pair problem).
/// Port of Go's `PreflightWarning` (core/livephoto.go), contract shape.
#[derive(Debug, Clone)]
pub struct PreflightWarning {
    pub file_path: PathBuf,
    pub message: String,
}

/// Album assembly progress. Port of Go's `AlbumStatus` (core/album.go),
/// contract shape.
#[derive(Debug, Clone)]
pub struct AlbumStatus {
    pub name: String,
    pub total: usize,
    pub done: usize,
}

/// Receives progress events from an upload run. Port of Go's
/// `UploadReporter` interface (core/reporter.go); method names and payloads
/// follow CONTRACT.md. Implementations must be cheap to call from worker
/// threads and must not block the upload pipeline.
pub trait UploadReporter: Send + Sync {
    fn upload_start(&self, total_files: usize);
    fn upload_stop(&self);
    fn total_bytes(&self, total: u64);
    fn total_bytes_delta(&self, delta: i64);
    fn warning(&self, w: PreflightWarning);
    fn thread_status(&self, s: ThreadStatus);
    fn file_result(&self, r: FileResult);
    fn album_progress(&self, s: AlbumStatus);
    fn album_complete(&self, s: AlbumStatus);
    fn album_error(&self, name: String, message: String);
}

/// Discards every event. Port of Go's `NopReporter` (core/reporter.go).
pub struct NullReporter;

impl UploadReporter for NullReporter {
    fn upload_start(&self, _total_files: usize) {}
    fn upload_stop(&self) {}
    fn total_bytes(&self, _total: u64) {}
    fn total_bytes_delta(&self, _delta: i64) {}
    fn warning(&self, _w: PreflightWarning) {}
    fn thread_status(&self, _s: ThreadStatus) {}
    fn file_result(&self, _r: FileResult) {}
    fn album_progress(&self, _s: AlbumStatus) {}
    fn album_complete(&self, _s: AlbumStatus) {}
    fn album_error(&self, _name: String, _message: String) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_token_clones_share_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn null_reporter_accepts_all_events() {
        let reporter = NullReporter;
        reporter.upload_start(2);
        reporter.total_bytes(10);
        reporter.total_bytes_delta(-3);
        reporter.warning(PreflightWarning {
            file_path: PathBuf::from("a.heic"),
            message: "unpaired".to_string(),
        });
        reporter.thread_status(ThreadStatus {
            worker: 0,
            stage: Stage::Hashing,
            file_path: PathBuf::from("a.heic"),
            bytes_uploaded: 0,
            bytes_total: 10,
            attempt: 0,
        });
        reporter.file_result(FileResult {
            file_path: PathBuf::from("a.heic"),
            outcome: Outcome::SkippedAlreadyPresent,
            media_key: Some("key".to_string()),
            message: None,
        });
        let album = AlbumStatus {
            name: "Trip".to_string(),
            total: 3,
            done: 1,
        };
        reporter.album_progress(album.clone());
        reporter.album_complete(album);
        reporter.album_error("Trip".to_string(), "boom".to_string());
        reporter.upload_stop();
    }
}
