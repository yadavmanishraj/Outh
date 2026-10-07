//! Upload engine — port of gotohp `core/upload.go` (manager, worker pool,
//! per-file pipeline, preflight filter) and `core/album.go` (album
//! orchestration), with `core/upload_options.go` for [`UploadOptions`].
//!
//! Pipeline per regular file (Go `uploadSingleFile`):
//! hash (SHA-1) → dedupe check (skip if already in the library, unless
//! `force_upload`) → get upload token → chunked PUT (progress) → commit →
//! optional delete-from-host (only after a *confirmed* media key).
//! Retry placement matches Go: the PUT retries up to 3 times *inside*
//! `PhotosClient::upload_file` (Go `UploadFileWithProgress`, re-streaming the
//! whole file per attempt); the commit is never retried by this layer, and an
//! ambiguous commit (2xx + unparseable body) is surfaced by the client as
//! `Error::CommitAmbiguous` and treated as a plain failure here — never
//! re-attempted, so no duplicates.
//!
//! Client construction: Go builds one `Api` per worker from the run's
//! `ApiOptions`. Here the manager holds a factory
//! `Arc<dyn Fn(&ApiOptions) -> Result<PhotosClient> + Send + Sync>` (chosen
//! over a single ready client because the active account can switch between
//! runs); the factory is called once per worker, once as a preflight
//! validation, and once for the album phase — mirroring Go's `NewApi` calls.
//!
//! Contract-shape notes (CONTRACT.md wins over Go where they differ):
//! - `ThreadStatus` has no message/file-name fields, so Go's status strings
//!   and "idle" statuses have no Rust counterpart and are not emitted.
//! - The client's progress callback is `Fn(u64, u64)` (no attempt number), so
//!   `ThreadStatus.attempt` is reported as 0 from this layer.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{FileMeta, PhotosClient, ScottyToken};
use crate::config::{ApiOptions, Preferences};
use crate::create_media_items::{
    LivePhotoCreateRequest, LivePhotoReconcileRequest, MediaTimestamp,
};
use crate::filename::parse_timestamp_from_filename;
use crate::livephoto::{classify_with_options, ClassifyOptions, WorkItem};
use crate::sha1calc::compute_sha1;
use crate::types::{
    AlbumStatus, CancellationToken, FileResult, Outcome, PreflightWarning, Stage, ThreadStatus,
    UploadReporter,
};
use crate::{Error, Result};

/// Maximum items per AddMediaToAlbum API call (album.go `AlbumBatchSize`).
const ALBUM_BATCH_SIZE: usize = 500;
/// Maximum items per album (album.go `AlbumLimit`); larger sets are split
/// into numbered albums.
const ALBUM_LIMIT: usize = 20_000;
/// Retries for album create/add calls (httpclient.go `DefaultRetryConfig`).
const ALBUM_MAX_RETRIES: u32 = 3;

// ---------------------------------------------------------------------------
// UploadOptions (upload_options.go)
// ---------------------------------------------------------------------------

/// Run options for one upload — mirrors Go `UploadOptions`.
/// Build from GUI preferences with [`UploadOptions::from_preferences`]
/// (Go `Preferences.UploadOptions()`), or construct directly (CLI-style).
#[derive(Clone, Debug)]
pub struct UploadOptions {
    /// Account + upload policy used for every request in the run.
    pub api: ApiOptions,
    pub recursive: bool,
    pub exclude_pattern: String,
    pub threads: u32,
    pub force_upload: bool,
    pub delete_from_host: bool,
    pub disable_unsupported_filter: bool,
    pub set_date_from_filename: bool,
    pub pair_live_photos: bool,
    pub skip_incomplete_live_photos: bool,
    pub update_existing_to_live: bool,
    /// Match Live Photo pairs by filename stem instead of Apple content
    /// identifiers. Never persisted (Go `IgnoreAppleMetadata`).
    pub ignore_apple_metadata: bool,
    /// Album name or album key to add uploads to. Ignored in auto mode.
    pub album_name: String,
    /// Create one album per source directory.
    pub album_auto_mode: bool,
}

impl UploadOptions {
    /// Derive run options from GUI preferences — Go
    /// `Preferences.UploadOptions()`. The returned value is a snapshot:
    /// later preference edits never affect a run already captured
    /// (asserted by the ported upload_options test).
    pub fn from_preferences(prefs: &Preferences) -> Self {
        UploadOptions {
            api: ApiOptions {
                proxy: prefs.proxy.clone(),
                saver: prefs.saver,
                use_quota: prefs.use_quota,
            },
            recursive: prefs.recursive,
            exclude_pattern: prefs.exclude_pattern.clone(),
            threads: prefs.upload_threads,
            force_upload: prefs.force_upload,
            delete_from_host: prefs.delete_from_host,
            disable_unsupported_filter: prefs.disable_unsupported_filter,
            set_date_from_filename: prefs.set_date_from_filename,
            pair_live_photos: prefs.pair_live_photos,
            skip_incomplete_live_photos: prefs.skip_incomplete_live_photos,
            update_existing_to_live: prefs.update_existing_to_live,
            ignore_apple_metadata: false,
            album_name: prefs.album_name.clone(),
            album_auto_mode: prefs.album_auto_mode,
        }
    }

    /// Go `normalized()`: at least one thread; auto mode clears the manual
    /// album name.
    pub fn normalized(mut self) -> Self {
        if self.threads < 1 {
            self.threads = 1;
        }
        if self.album_auto_mode {
            self.album_name.clear();
        }
        self
    }
}

// ---------------------------------------------------------------------------
// UploadManager (upload.go)
// ---------------------------------------------------------------------------

/// Counts for one finished run (the Rust run() is synchronous where Go's
/// `Upload` returns immediately and reports through the reporter; the
/// reporter event stream is unchanged).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub total_items: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

/// Factory for per-worker clients (see module docs).
pub type PhotosFactory = Arc<dyn Fn(&ApiOptions) -> Result<PhotosClient> + Send + Sync>;

pub struct UploadManager {
    factory: PhotosFactory,
    reporter: Arc<dyn UploadReporter>,
    running: AtomicBool,
}

/// Emits `upload_stop` exactly once when the run ends, on every exit path —
/// Go pairs UploadStart/UploadStop through `finish()`.
struct StopGuard<'a> {
    reporter: &'a Arc<dyn UploadReporter>,
}

impl Drop for StopGuard<'_> {
    fn drop(&mut self) {
        self.reporter.upload_stop();
    }
}

struct RunningGuard<'a>(&'a AtomicBool);

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl UploadManager {
    pub fn new(factory: PhotosFactory, reporter: Arc<dyn UploadReporter>) -> Self {
        UploadManager {
            factory,
            reporter,
            running: AtomicBool::new(false),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Run one upload batch to completion. A second call while a run is in
    /// progress is a no-op returning an empty summary (Go behaviour).
    pub fn run(
        &self,
        inputs: Vec<PathBuf>,
        opts: UploadOptions,
        cancel: CancellationToken,
    ) -> RunSummary {
        let opts = opts.normalized();
        if self.running.swap(true, Ordering::SeqCst) {
            return RunSummary::default();
        }
        let _running = RunningGuard(&self.running);
        let mut summary = RunSummary::default();

        // Make preflight visible immediately so a long scan can be cancelled
        // from the UI (Go emits UploadStart with an empty batch first).
        self.reporter.upload_start(0);
        let _stop = StopGuard {
            reporter: &self.reporter,
        };

        // ---- Preflight: expand + filter inputs (upload.go filter). ----
        let target_paths = match filter_files(&inputs, &opts, &cancel) {
            Ok(paths) => paths,
            Err(Error::Cancelled) => {
                summary.cancelled = true;
                return summary;
            }
            Err(e) => {
                self.reporter.file_result(FileResult {
                    file_path: PathBuf::new(),
                    outcome: Outcome::Failed,
                    media_key: None,
                    message: Some(e.to_string()),
                });
                summary.failed = 1;
                return summary;
            }
        };

        // ---- Live Photo classification (livephoto.go ClassifyUploadWork). ----
        let class_opts = ClassifyOptions {
            pair_live_photos: opts.pair_live_photos,
            skip_incomplete: opts.skip_incomplete_live_photos,
            ignore_apple_metadata: opts.ignore_apple_metadata,
        };
        // livephoto's classify API deliberately has no cancellation
        // knob (see its module docs); the engine checks its token
        // immediately around the call instead.
        let classified = classify_with_options(&target_paths, &class_opts);
        let work_items = classified.items;
        let warnings = classified.warnings;
        if cancel.is_cancelled() {
            summary.cancelled = true;
            return summary;
        }

        // Go's reportPreflight: a second UploadStart with the real total,
        // then every warning. (Go also synthesises skipped FileResults for
        // incomplete/ambiguous-stem warnings from their Code+Paths; the
        // contract's PreflightWarning carries neither, so those items simply
        // never enter the queue here.)
        self.reporter.upload_start(work_items.len());
        for warning in warnings {
            self.reporter.warning(warning);
        }
        summary.total_items = work_items.len();
        if work_items.is_empty() {
            return summary;
        }

        // ---- Validate client construction once, like Go's NewApi check. ----
        if let Err(e) = (self.factory)(&opts.api) {
            for item in &work_items {
                self.reporter.file_result(FileResult {
                    file_path: work_primary_path(item),
                    outcome: Outcome::Failed,
                    media_key: None,
                    message: Some(e.to_string()),
                });
            }
            summary.failed = work_items.len();
            return summary;
        }

        // ---- Total bytes (Go computes this in a side goroutine and emits
        // TotalBytes when ready; done synchronously before dispatch here —
        // the reporter sees the same value, just not interleaved). ----
        let mut total_bytes: u64 = 0;
        for item in &work_items {
            for path in work_paths(item) {
                if let Ok(info) = fs::metadata(&path) {
                    total_bytes += info.len();
                }
            }
        }
        self.reporter.total_bytes(total_bytes);

        // ---- Worker pool: std::thread workers over an mpsc queue. ----
        let num_workers = std::cmp::min(opts.threads as usize, work_items.len());
        let (work_tx, work_rx) = mpsc::channel::<WorkItem>();
        // std mpsc receivers are single-consumer; share under a mutex. The
        // lock is held only for the recv call itself, so workers process in
        // parallel.
        let work_rx = Arc::new(Mutex::new(work_rx));
        let (res_tx, res_rx) = mpsc::channel::<FileResult>();
        let mut successes: Vec<(PathBuf, String)> = Vec::new();

        // Bind references up front so the `move` closures below capture the
        // (Copy) references, not the values themselves.
        let opts_ref = &opts;
        let cancel_ref = &cancel;
        let factory_ref = &self.factory;
        let reporter_ref = &self.reporter;
        std::thread::scope(|scope| {
            for worker in 0..num_workers {
                let work_rx = Arc::clone(&work_rx);
                let res_tx = res_tx.clone();
                scope.spawn(move || {
                    run_worker(
                        worker, &work_rx, &res_tx, opts_ref, cancel_ref, factory_ref,
                        reporter_ref,
                    );
                });
            }
            drop(res_tx);

            // Dispatch; cancellation stops dispatching (Go breaks its send
            // loop on cancel, then workers drain/exit).
            for item in work_items {
                if cancel.is_cancelled() {
                    break;
                }
                if work_tx.send(item).is_err() {
                    break;
                }
            }
            drop(work_tx);

            // Collect results until every worker's sender is dropped.
            for result in res_rx {
                if result.outcome != Outcome::Failed {
                    if let Some(key) = &result.media_key {
                        successes.push((result.file_path.clone(), key.clone()));
                    }
                }
                match result.outcome {
                    Outcome::Uploaded => summary.uploaded += 1,
                    Outcome::SkippedAlreadyPresent | Outcome::SkippedUnsupported => {
                        summary.skipped += 1
                    }
                    Outcome::Failed => summary.failed += 1,
                }
                self.reporter.file_result(result);
            }
        });
        if cancel.is_cancelled() {
            summary.cancelled = true;
        }

        // ---- Album phase (upload.go handleAlbumCreation). ----
        if !successes.is_empty() && !cancel.is_cancelled() {
            match (self.factory)(&opts.api) {
                Ok(client) => {
                    let albums = AlbumRunner {
                        client: &client,
                        reporter: &self.reporter,
                        cancel: &cancel,
                    };
                    albums.handle(&successes, &opts);
                }
                Err(e) => {
                    self.reporter.album_error(
                        opts.album_name.clone(),
                        format!("failed to initialize API: {e}"),
                    );
                }
            }
        }

        summary
    }
}

fn run_worker(
    worker: usize,
    work_rx: &Arc<Mutex<mpsc::Receiver<WorkItem>>>,
    res_tx: &mpsc::Sender<FileResult>,
    opts: &UploadOptions,
    cancel: &CancellationToken,
    factory: &PhotosFactory,
    reporter: &Arc<dyn UploadReporter>,
) {
    // One client per worker for connection reuse (Go runWorker).
    let client = match factory(&opts.api) {
        Ok(client) => client,
        Err(e) => {
            reporter.thread_status(ThreadStatus {
                worker,
                stage: Stage::Error,
                file_path: PathBuf::new(),
                bytes_uploaded: 0,
                bytes_total: 0,
                attempt: 0,
            });
            drop(e);
            return;
        }
    };

    loop {
        let item = {
            let rx = match work_rx.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            rx.recv()
        };
        let item = match item {
            Ok(item) => item,
            Err(_) => return, // Queue closed and drained.
        };
        if cancel.is_cancelled() {
            return;
        }
        let result = process_item(&client, &item, opts, worker, reporter, cancel);
        if res_tx.send(result).is_err() {
            return;
        }
    }
}

fn process_item(
    client: &PhotosClient,
    item: &WorkItem,
    opts: &UploadOptions,
    worker: usize,
    reporter: &Arc<dyn UploadReporter>,
    cancel: &CancellationToken,
) -> FileResult {
    match item {
        WorkItem::Single(path) => {
            let path = path.clone();
            upload_single(client, &path, opts, worker, reporter, cancel)
        }
        WorkItem::LivePhoto { still, video, .. } => {
            upload_live_photo(client, still, video, opts, worker, reporter, cancel)
        }
    }
}

// ---------------------------------------------------------------------------
// Per-file pipeline (upload.go uploadSingleFile)
// ---------------------------------------------------------------------------

fn emit_status(
    reporter: &Arc<dyn UploadReporter>,
    worker: usize,
    stage: Stage,
    path: &Path,
    bytes_uploaded: u64,
    bytes_total: u64,
) {
    reporter.thread_status(ThreadStatus {
        worker,
        stage,
        file_path: path.to_path_buf(),
        bytes_uploaded,
        bytes_total,
        attempt: 0,
    });
}

fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn mtime_unix_secs(path: &Path) -> i64 {
    if let Ok(info) = fs::metadata(path) {
        if let Ok(modified) = info.modified() {
            return match modified.duration_since(UNIX_EPOCH) {
                Ok(d) => d.as_secs() as i64,
                Err(e) => -(e.duration().as_secs() as i64),
            };
        }
    }
    0
}

/// Upload timestamp for a file: mtime by default; a filename-derived date
/// wins when `set_date_from_filename` is on (upload.go).
fn taken_unix_secs(path: &Path, opts: &UploadOptions) -> i64 {
    let mut taken = mtime_unix_secs(path);
    if opts.set_date_from_filename {
        if let Some(ts) = parse_timestamp_from_filename(&path.to_string_lossy()) {
            taken = ts;
        }
    }
    taken
}

fn failed_result(path: &Path, message: String) -> FileResult {
    FileResult {
        file_path: path.to_path_buf(),
        outcome: Outcome::Failed,
        media_key: None,
        message: Some(message),
    }
}

fn upload_single(
    client: &PhotosClient,
    path: &Path,
    opts: &UploadOptions,
    worker: usize,
    reporter: &Arc<dyn UploadReporter>,
    cancel: &CancellationToken,
) -> FileResult {
    let taken = taken_unix_secs(path, opts);

    // Stage 1: hashing.
    emit_status(reporter, worker, Stage::Hashing, path, 0, 0);
    let sha1 = match compute_sha1(path, cancel) {
        Ok(hash) => hash,
        Err(e) => return failed_result(path, format!("error calculating hash file: {e}")),
    };

    // Stage 2: dedupe check (skipped under force_upload). A failed lookup is
    // NON-fatal in Go (it logs a warning status and proceeds); preserved.
    if !opts.force_upload {
        emit_status(reporter, worker, Stage::Checking, path, 0, 0);
        if let Ok(Some(media_key)) = client.find_remote_media_by_hash(&sha1) {
            emit_status(reporter, worker, Stage::Completed, path, 0, 0);
            if opts.delete_from_host {
                if let Err(e) = fs::remove_file(path) {
                    let message =
                        format!("file exists in library but failed to delete local copy: {e}");
                    reporter.warning(PreflightWarning {
                        file_path: path.to_path_buf(),
                        message: format!("local-cleanup-failed: {message}"),
                    });
                    return FileResult {
                        file_path: path.to_path_buf(),
                        outcome: Outcome::SkippedAlreadyPresent,
                        media_key: Some(media_key),
                        message: Some(message),
                    };
                }
            }
            return FileResult {
                file_path: path.to_path_buf(),
                outcome: Outcome::SkippedAlreadyPresent,
                media_key: Some(media_key),
                message: None,
            };
        }
    }

    let size = match fs::metadata(path) {
        Ok(info) => info.len(),
        Err(e) => return failed_result(path, format!("error getting file info: {e}")),
    };

    // Stage 3: token + chunked PUT with progress. The PUT's retries (max 3,
    // whole-file re-stream) live inside the client, as in Go's
    // UploadFileWithProgress; CommitAmbiguous can never occur here.
    emit_status(reporter, worker, Stage::Uploading, path, 0, size);
    let session = match client.get_upload_token(&sha1, size) {
        Ok(session) => session,
        Err(e) => return failed_result(path, format!("error uploading file: {e}")),
    };
    let progress_reporter = Arc::clone(reporter);
    let progress_path = path.to_path_buf();
    let progress = move |uploaded: u64, total: u64| {
        progress_reporter.thread_status(ThreadStatus {
            worker,
            stage: Stage::Uploading,
            file_path: progress_path.clone(),
            bytes_uploaded: uploaded,
            bytes_total: total,
            attempt: 0,
        });
    };
    let token = match client.upload_file(&session, path, &progress, cancel) {
        Ok(token) => token,
        Err(e) => return failed_result(path, format!("error uploading file: {e}")),
    };

    // Stage 4: commit. Never retried by this layer (see module docs).
    emit_status(reporter, worker, Stage::Finalizing, path, size, size);
    let meta = FileMeta {
        name: file_name_string(path),
        size,
        sha1,
        taken_unix_secs: taken,
    };
    let media_key = match client.commit_upload(&token, &meta) {
        Ok(key) => key,
        Err(e) => return failed_result(path, format!("error committing file: {e}")),
    };
    if media_key.is_empty() {
        return failed_result(path, "media key not received".to_string());
    }
    emit_status(reporter, worker, Stage::Completed, path, size, size);

    // Delete from host ONLY after a confirmed media key (upload.go).
    if opts.delete_from_host {
        if let Err(e) = fs::remove_file(path) {
            let message = format!("uploaded successfully but failed to delete file: {e}");
            reporter.warning(PreflightWarning {
                file_path: path.to_path_buf(),
                message: format!("local-cleanup-failed: {message}"),
            });
            return FileResult {
                file_path: path.to_path_buf(),
                outcome: Outcome::Uploaded,
                media_key: Some(media_key),
                message: Some(message),
            };
        }
    }

    FileResult {
        file_path: path.to_path_buf(),
        outcome: Outcome::Uploaded,
        media_key: Some(media_key),
        message: None,
    }
}

// ---------------------------------------------------------------------------
// Live Photo pipeline (livephoto_upload.go)
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn upload_component(
    client: &PhotosClient,
    component_path: &Path,
    progress_path: &Path,
    sha1: &[u8; 20],
    size: u64,
    completed_before: u64,
    pair_total: u64,
    worker: usize,
    reporter: &Arc<dyn UploadReporter>,
    cancel: &CancellationToken,
) -> Result<ScottyToken> {
    let session = client.get_upload_token(sha1, size)?;
    let progress_reporter = Arc::clone(reporter);
    let progress_path = progress_path.to_path_buf();
    let progress = move |uploaded: u64, _total: u64| {
        progress_reporter.thread_status(ThreadStatus {
            worker,
            stage: Stage::Uploading,
            file_path: progress_path.clone(),
            bytes_uploaded: completed_before + uploaded,
            bytes_total: pair_total,
            attempt: 0,
        });
    };
    client.upload_file(&session, component_path, &progress, cancel)
}

/// Delete a Live Photo pair after a confirmed commit. Go removes the video
/// first, then the still (livephoto_upload.go removeLivePhotoFiles).
fn remove_live_photo_files(photo_path: &Path, video_path: &Path) -> Result<()> {
    for path in [video_path, photo_path] {
        fs::remove_file(path).map_err(|e| {
            Error::Other(format!(
                "Live Photo uploaded but failed to delete {}: {e}",
                file_name_string(path)
            ))
        })?;
    }
    Ok(())
}

fn upload_live_photo(
    client: &PhotosClient,
    photo_path: &Path,
    video_path: &Path,
    opts: &UploadOptions,
    worker: usize,
    reporter: &Arc<dyn UploadReporter>,
    cancel: &CancellationToken,
) -> FileResult {
    let photo = photo_path.to_path_buf();
    let video = video_path.to_path_buf();

    let photo_size = match fs::metadata(&photo) {
        Ok(info) => info.len(),
        Err(e) => return failed_result(&photo, format!("stat Live Photo still: {e}")),
    };
    let video_size = match fs::metadata(&video) {
        Ok(info) => info.len(),
        Err(e) => return failed_result(&photo, format!("stat Live Photo video: {e}")),
    };
    let total = photo_size + video_size;

    emit_status(reporter, worker, Stage::Hashing, &photo, 0, total);
    let photo_sha1 = match compute_sha1(&photo, cancel) {
        Ok(hash) => hash,
        Err(e) => return failed_result(&photo, format!("hash Live Photo still: {e}")),
    };
    let video_sha1 = match compute_sha1(&video, cancel) {
        Ok(hash) => hash,
        Err(e) => return failed_result(&photo, format!("hash Live Photo video: {e}")),
    };

    // Unlike the single-file path, dedupe lookup errors are FATAL for Live
    // Photos in Go; preserved.
    emit_status(reporter, worker, Stage::Checking, &photo, 0, total);
    let photo_remote = match client.find_remote_media_by_hash(&photo_sha1) {
        Ok(key) => key,
        Err(e) => {
            return failed_result(&photo, format!("check Live Photo still deduplication: {e}"))
        }
    };
    let video_remote = match client.find_remote_media_by_hash(&video_sha1) {
        Ok(key) => key,
        Err(e) => {
            return failed_result(&photo, format!("check Live Photo video deduplication: {e}"))
        }
    };

    if photo_remote.is_some() {
        if opts.update_existing_to_live {
            // Go: the still's bytes leave the batch total (it is not
            // re-uploaded); the video is uploaded and reconciled.
            reporter.total_bytes_delta(-(photo_size as i64));
            return reconcile_live_photo(
                client,
                &photo,
                &video,
                &photo_sha1,
                &video_sha1,
                video_size,
                opts,
                worker,
                reporter,
                cancel,
            );
        }
        reporter.total_bytes_delta(-(total as i64));
        emit_status(reporter, worker, Stage::Skipped, &photo, 0, total);
        return FileResult {
            file_path: photo,
            outcome: Outcome::SkippedAlreadyPresent,
            media_key: None,
            message: Some(
                "Skipped because a Live Photo component already exists remotely".to_string(),
            ),
        };
    }
    // A standalone remote MOV must not be combined with a newly uploaded
    // still using the Phodeo reconcile request (livephoto_upload.go).
    if video_remote.is_some() {
        reporter.total_bytes_delta(-(total as i64));
        emit_status(reporter, worker, Stage::Skipped, &photo, 0, total);
        return FileResult {
            file_path: photo,
            outcome: Outcome::SkippedAlreadyPresent,
            media_key: None,
            message: Some("Skipped because the Live Photo video already exists remotely".to_string()),
        };
    }

    let photo_token = match upload_component(
        client,
        &photo,
        &photo,
        &photo_sha1,
        photo_size,
        0,
        total,
        worker,
        reporter,
        cancel,
    ) {
        Ok(token) => token,
        Err(e) => return failed_result(&photo, format!("upload Live Photo still: {e}")),
    };
    let video_token = match upload_component(
        client,
        &video,
        &photo,
        &video_sha1,
        video_size,
        photo_size,
        total,
        worker,
        reporter,
        cancel,
    ) {
        Ok(token) => token,
        Err(e) => return failed_result(&photo, format!("upload Live Photo video: {e}")),
    };

    let taken = taken_unix_secs(&photo, opts);
    emit_status(reporter, worker, Stage::Finalizing, &photo, total, total);
    let policy = client.live_photo_commit_policy();
    let media_key = match client.commit_live_photo(LivePhotoCreateRequest {
        photo_token,
        video_token,
        file_name: file_name_string(&photo),
        photo_sha1: photo_sha1.to_vec(),
        video_sha1: video_sha1.to_vec(),
        created_at: MediaTimestamp::from_unix_secs(taken),
        modified_at: MediaTimestamp::from_unix_secs(taken),
        storage_policy: policy.storage_policy,
        upload_quality: policy.upload_quality,
        upload_device_info: policy.upload_device_info,
    }) {
        Ok(key) => key,
        Err(e) => return failed_result(&photo, format!("commit Live Photo: {e}")),
    };
    if media_key.is_empty() {
        return failed_result(&photo, "Live Photo media key not received".to_string());
    }
    emit_status(reporter, worker, Stage::Completed, &photo, total, total);

    if opts.delete_from_host {
        if let Err(e) = remove_live_photo_files(&photo, &video) {
            reporter.warning(PreflightWarning {
                file_path: photo.clone(),
                message: format!("local-cleanup-failed: {e}"),
            });
            return FileResult {
                file_path: photo,
                outcome: Outcome::Uploaded,
                media_key: Some(media_key),
                message: Some(e.to_string()),
            };
        }
    }

    FileResult {
        file_path: photo,
        outcome: Outcome::Uploaded,
        media_key: Some(media_key),
        message: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn reconcile_live_photo(
    client: &PhotosClient,
    photo_path: &Path,
    video_path: &Path,
    photo_sha1: &[u8; 20],
    video_sha1: &[u8; 20],
    video_size: u64,
    opts: &UploadOptions,
    worker: usize,
    reporter: &Arc<dyn UploadReporter>,
    cancel: &CancellationToken,
) -> FileResult {
    let photo = photo_path.to_path_buf();
    let video = video_path.to_path_buf();

    emit_status(reporter, worker, Stage::Uploading, &photo, 0, video_size);
    // Go reports progress against the video alone on this path.
    let video_token = match upload_component(
        client,
        &video,
        &photo,
        video_sha1,
        video_size,
        0,
        video_size,
        worker,
        reporter,
        cancel,
    ) {
        Ok(token) => token,
        Err(e) => {
            return failed_result(
                &photo,
                format!("upload Live Photo video for existing photo: {e}"),
            )
        }
    };

    let taken = taken_unix_secs(&photo, opts);
    emit_status(reporter, worker, Stage::Finalizing, &photo, video_size, video_size);
    let policy = client.live_photo_commit_policy();
    let media_key = match client.reconcile_live_photo(LivePhotoReconcileRequest {
        video_token,
        // Go uses the VIDEO's file name for the reconcile request.
        file_name: file_name_string(&video),
        photo_sha1: photo_sha1.to_vec(),
        video_sha1: video_sha1.to_vec(),
        created_at: MediaTimestamp::from_unix_secs(taken),
        modified_at: MediaTimestamp::from_unix_secs(taken),
        storage_policy: policy.storage_policy,
        upload_quality: policy.upload_quality,
        upload_device_info: policy.upload_device_info,
    }) {
        Ok(key) => key,
        Err(e) => return failed_result(&photo, format!("update existing photo to Live: {e}")),
    };
    if media_key.is_empty() {
        return failed_result(&photo, "updated Live Photo media key not received".to_string());
    }
    emit_status(reporter, worker, Stage::Completed, &photo, video_size, video_size);

    if opts.delete_from_host {
        if let Err(e) = remove_live_photo_files(&photo, &video) {
            reporter.warning(PreflightWarning {
                file_path: photo.clone(),
                message: format!("local-cleanup-failed: {e}"),
            });
            return FileResult {
                file_path: photo,
                outcome: Outcome::Uploaded,
                media_key: Some(media_key),
                message: Some(e.to_string()),
            };
        }
    }

    FileResult {
        file_path: photo,
        outcome: Outcome::Uploaded,
        media_key: Some(media_key),
        message: None,
    }
}

// ---------------------------------------------------------------------------
// Album orchestration (album.go)
// ---------------------------------------------------------------------------

/// An input is an album key if it starts with "AF1Qip" (album.go IsAlbumKey).
fn is_album_key(input: &str) -> bool {
    input.len() > 6 && input.starts_with("AF1Qip")
}

/// Go `isRetryableAlbumError`: textual match on the error — 5xx / 429 /
/// connection / timeout. `Error::AlbumNotFound` (404) never matches, so the
/// album-not-found distinction survives to `album_error`.
fn is_retryable_album_error(err: &Error) -> bool {
    let s = err.to_string();
    s.contains("status 5") || s.contains("status 429") || s.contains("connection") || s.contains("timeout")
}

/// Go `CalculateBackoff`: 1s doubling, capped at 30s, plus up to 10% jitter.
/// Sleeps in short slices so cancellation lands promptly; returns false if
/// cancelled while waiting.
fn sleep_backoff(attempt: u32, cancel: &CancellationToken) -> bool {
    let mut delay_ms: u64 = 1_000u64 << attempt.min(20);
    delay_ms = delay_ms.min(30_000);
    // Jitter without a rand dependency: nanosecond clock entropy, same bound
    // as Go (delay/10).
    let jitter_bound = (delay_ms / 10).max(1);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    delay_ms += nanos % jitter_bound;

    let mut remaining = delay_ms;
    while remaining > 0 {
        if cancel.is_cancelled() {
            return false;
        }
        let slice = remaining.min(100);
        std::thread::sleep(std::time::Duration::from_millis(slice));
        remaining -= slice;
    }
    !cancel.is_cancelled()
}

struct AlbumRunner<'a> {
    client: &'a PhotosClient,
    reporter: &'a Arc<dyn UploadReporter>,
    cancel: &'a CancellationToken,
}

impl AlbumRunner<'_> {
    /// upload.go handleAlbumCreation + createAlbumsFromDirectories.
    fn handle(&self, successes: &[(PathBuf, String)], opts: &UploadOptions) {
        if opts.album_auto_mode {
            // One album per source directory, named after the directory
            // (first-seen order here; Go iterates a map, i.e. unordered).
            let mut groups: Vec<(PathBuf, Vec<String>)> = Vec::new();
            for (path, key) in successes {
                let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                match groups.iter_mut().find(|(d, _)| *d == dir) {
                    Some((_, keys)) => keys.push(key.clone()),
                    None => groups.push((dir, vec![key.clone()])),
                }
            }
            for (dir, keys) in groups {
                let name = dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Uploads".to_string());
                if let Err(e) = self.add_to_album(&keys, &name) {
                    self.reporter.album_error(name, e.to_string());
                }
            }
            return;
        }

        if opts.album_name.is_empty() {
            return;
        }
        let keys: Vec<String> = successes.iter().map(|(_, k)| k.clone()).collect();
        if let Err(e) = self.add_to_album(&keys, &opts.album_name) {
            self.reporter.album_error(opts.album_name.clone(), e.to_string());
        }
    }

    /// album.go AddToAlbum: an album key adds to the existing album; any
    /// other string creates new album(s) with that name.
    fn add_to_album(&self, media_keys: &[String], name_or_key: &str) -> Result<Vec<String>> {
        let name_or_key = name_or_key.trim();
        if media_keys.is_empty() {
            return Err(Error::Other("no media keys provided".to_string()));
        }
        if name_or_key.is_empty() {
            return Err(Error::Other("album name or key cannot be empty".to_string()));
        }
        if is_album_key(name_or_key) {
            return self.add_to_existing(media_keys, name_or_key);
        }
        self.create_new(media_keys, name_or_key)
    }

    fn add_to_existing(&self, media_keys: &[String], album_key: &str) -> Result<Vec<String>> {
        let display = format!(
            "Album ({}...)",
            album_key.chars().take(10).collect::<String>()
        );
        let total = media_keys.len();
        let mut done = 0usize;
        for batch in media_keys.chunks(ALBUM_BATCH_SIZE) {
            if self.cancel.is_cancelled() {
                return Err(Error::Other(format!(
                    "album creation cancelled (added {done}/{total} items)"
                )));
            }
            self.add_with_retry(album_key, batch).map_err(|e| {
                Error::Other(format!(
                    "failed to add media to album (added {done}/{total} items): {e}"
                ))
            })?;
            done += batch.len();
            self.reporter.album_progress(AlbumStatus {
                name: display.clone(),
                total,
                done,
            });
        }
        self.reporter.album_complete(AlbumStatus {
            name: display,
            total,
            done,
        });
        Ok(vec![album_key.to_string()])
    }

    fn create_new(&self, media_keys: &[String], album_name: &str) -> Result<Vec<String>> {
        let total = media_keys.len();
        let mut album_keys: Vec<String> = Vec::new();
        let mut done = 0usize;
        let mut counter = 1usize;

        for chunk in media_keys.chunks(ALBUM_LIMIT) {
            if self.cancel.is_cancelled() {
                return Err(Error::Other(format!(
                    "album creation cancelled (added {done}/{total} items)"
                )));
            }
            // Numbered suffixes when the set exceeds one album (album.go).
            let current_name = if total > ALBUM_LIMIT {
                format!("{album_name} ({counter})")
            } else {
                album_name.to_string()
            };

            let mut current_key: Option<String> = None;
            for batch in chunk.chunks(ALBUM_BATCH_SIZE) {
                if self.cancel.is_cancelled() {
                    return Err(Error::Other(format!(
                        "album creation cancelled (added {done}/{total} items)"
                    )));
                }
                if current_key.is_none() {
                    // Contract shape: create_album(name) creates the album
                    // and returns its key; batches are then added with
                    // add_media_to_album. (Go's CreateAlbum takes the first
                    // batch in the same call — one extra API call here, same
                    // end state.)
                    let key = self.create_with_retry(&current_name).map_err(|e| {
                        Error::Other(format!(
                            "failed to create album '{current_name}' (added {done}/{total} items): {e}"
                        ))
                    })?;
                    album_keys.push(key.clone());
                    current_key = Some(key);
                }
                let key = current_key.clone().expect("album key set above");
                self.add_with_retry(&key, batch).map_err(|e| {
                    Error::Other(format!(
                        "failed to add media to album '{current_name}' (added {done}/{total} items): {e}"
                    ))
                })?;
                done += batch.len();
                self.reporter.album_progress(AlbumStatus {
                    name: current_name.clone(),
                    total,
                    done,
                });
            }
            counter += 1;
        }

        self.reporter.album_complete(AlbumStatus {
            name: album_name.to_string(),
            total,
            done,
        });
        Ok(album_keys)
    }

    /// album.go addMediaWithRetry: up to 3 retries with backoff, only for
    /// retryable errors.
    fn add_with_retry(&self, album_key: &str, batch: &[String]) -> Result<()> {
        let mut last_err: Option<Error> = None;
        for attempt in 0..=ALBUM_MAX_RETRIES {
            if attempt > 0 {
                if self.cancel.is_cancelled() || !sleep_backoff(attempt - 1, self.cancel) {
                    return Err(Error::Other("cancelled during retry".to_string()));
                }
            }
            match self.client.add_media_to_album(album_key, batch) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if !is_retryable_album_error(&e) {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(Error::Other(format!(
            "failed after {} attempts: {}",
            ALBUM_MAX_RETRIES + 1,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// album.go createAlbumWithRetry.
    fn create_with_retry(&self, album_name: &str) -> Result<String> {
        let mut last_err: Option<Error> = None;
        for attempt in 0..=ALBUM_MAX_RETRIES {
            if attempt > 0 {
                if self.cancel.is_cancelled() || !sleep_backoff(attempt - 1, self.cancel) {
                    return Err(Error::Other("cancelled during retry".to_string()));
                }
            }
            match self.client.create_album(album_name) {
                Ok(key) => return Ok(key),
                Err(e) => {
                    if !is_retryable_album_error(&e) {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(Error::Other(format!(
            "failed after {} attempts: {}",
            ALBUM_MAX_RETRIES + 1,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
}

// ---------------------------------------------------------------------------
// Preflight filter (upload.go filterGooglePhotosFilesWithCancel + scan)
// ---------------------------------------------------------------------------

/// Expand input paths into the files that would be uploaded under `opts`:
/// directories are scanned, unsupported files dropped, duplicates removed.
/// Go `FilterGooglePhotosFiles` (with a never-cancelled token).
pub fn filter_google_photos_files(inputs: &[PathBuf], opts: &UploadOptions) -> Result<Vec<PathBuf>> {
    filter_files(inputs, opts, &CancellationToken::new())
}

fn filter_files(
    inputs: &[PathBuf],
    opts: &UploadOptions,
    cancel: &CancellationToken,
) -> Result<Vec<PathBuf>> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen_keys: Vec<String> = Vec::new();

    for input in inputs {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let info = fs::metadata(input).map_err(|e| {
            Error::Other(format!("error accessing path {}: {e}", input.display()))
        })?;

        if info.is_dir() {
            let files = scan_directory(input, opts.recursive, &opts.exclude_pattern, cancel, true)
                .map_err(|e| match e {
                    Error::Cancelled => Error::Cancelled,
                    other => Error::Other(format!(
                        "error scanning directory {}: {other}",
                        input.display()
                    )),
                })?;
            for file in files {
                append_file(&mut out, &mut seen_keys, file, opts);
            }
        } else {
            append_file(&mut out, &mut seen_keys, input.clone(), opts);
        }
    }
    Ok(out)
}

fn append_file(
    out: &mut Vec<PathBuf>,
    seen_keys: &mut Vec<String>,
    path: PathBuf,
    opts: &UploadOptions,
) {
    if !opts.disable_unsupported_filter && !is_supported(&path) {
        return;
    }
    // Dedupe on the canonical path (Go: filepath.Abs + EvalSymlinks, bucket
    // lower-cased on Windows). Go additionally collapses hardlinks via
    // os.SameFile; std has no portable same-file test, so canonical-path
    // equality is the approximation (symlink duplicates still collapse).
    let canonical = canonical_path(&path);
    let key = if cfg!(windows) {
        canonical.to_string_lossy().to_lowercase()
    } else {
        canonical.to_string_lossy().into_owned()
    };
    if seen_keys.iter().any(|k| *k == key) {
        return;
    }
    seen_keys.push(key);
    out.push(path);
}

fn canonical_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    if let Ok(resolved) = fs::canonicalize(&absolute) {
        return resolved;
    }
    absolute
}

/// Go scanDirectoryForFiles. Note the faithful quirk: `exclude_pattern` is
/// compared against directory NAMES only (exact match), never applied to
/// files. Subdirectory scan errors are swallowed (Go `continue`); only a
/// failure to read the root directory (or cancellation) propagates.
fn scan_directory(
    path: &Path,
    recursive: bool,
    exclude_pattern: &str,
    cancel: &CancellationToken,
    is_root: bool,
) -> Result<Vec<PathBuf>> {
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let read_dir = match fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) => {
            if is_root {
                return Err(Error::Io(e));
            }
            return Ok(Vec::new());
        }
    };

    // Go's os.ReadDir returns entries sorted by filename.
    let mut entries: Vec<(std::ffi::OsString, PathBuf, bool)> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push((entry.file_name(), entry.path(), is_dir));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut files = Vec::new();
    for (name, full_path, is_dir) in entries {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if is_dir {
            if !exclude_pattern.is_empty() && name.to_string_lossy() == exclude_pattern {
                continue;
            }
            if recursive {
                match scan_directory(&full_path, recursive, exclude_pattern, cancel, false) {
                    Ok(mut sub) => files.append(&mut sub),
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(_) => continue,
                }
            }
        } else {
            files.push(full_path);
        }
    }
    Ok(files)
}

/// Google Photos supported extensions (upload.go supportedFormats).
fn is_supported(path: &Path) -> bool {
    let name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return false,
    };
    // Go uses filepath.Ext (final dot of the final element, so ".jpg" as a
    // whole filename still yields extension "jpg"); Path::extension differs
    // on dotfiles, hence the manual scan.
    let dot = match name.rfind('.') {
        Some(i) if i + 1 < name.len() => i,
        _ => return false,
    };
    let ext = name[dot + 1..].to_lowercase();
    matches!(
        ext.as_str(),
        // Photo formats
        "avif" | "bmp" | "gif" | "heic" | "heif" | "ico" | "jpg" | "jpeg" | "png" | "tif"
            | "tiff" | "webp" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "raf" | "rw2" | "pef"
            | "sr2" | "dng"
            // Video formats
            | "3gp" | "3g2" | "asf" | "avi" | "divx" | "m2t" | "m2ts" | "m4v" | "mkv" | "mmv"
            | "mod" | "mov" | "mp4" | "mpg" | "mpeg" | "mts" | "tod" | "wmv" | "ts" | "webm"
    )
}

// ---------------------------------------------------------------------------
// Work-item helpers (upload.go uploadWorkPaths / uploadWorkPrimaryPath)
// ---------------------------------------------------------------------------

fn work_paths(item: &WorkItem) -> Vec<PathBuf> {
    item.paths().into_iter().map(|p| p.to_path_buf()).collect()
}

fn work_primary_path(item: &WorkItem) -> PathBuf {
    item.primary_path().to_path_buf()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn base_options(prefs: &Preferences) -> UploadOptions {
        UploadOptions::from_preferences(prefs)
    }

    /// Port of upload_options_test.go
    /// TestSessionUploadOptionsCapturePreferences: session options snapshot
    /// the preferences at capture time; later preference changes affect only
    /// the NEXT capture. (Go drives this through ConfigManager setters over
    /// a loaded config; here Preferences is a plain value, which is exactly
    /// the snapshot semantics being asserted.)
    #[test]
    fn session_upload_options_capture_preferences() {
        let mut prefs = Preferences {
            proxy: "http://proxy.example:8080".to_string(),
            saver: true,
            use_quota: true,
            upload_threads: 7,
            pair_live_photos: true,
            skip_incomplete_live_photos: false,
            update_existing_to_live: true,
            album_name: "Holiday".to_string(),
            ..Default::default()
        };

        let captured = base_options(&prefs);
        assert_eq!(captured.api.proxy, "http://proxy.example:8080");
        assert!(captured.api.saver);
        assert!(captured.api.use_quota);
        assert_eq!(captured.threads, 7);
        assert!(captured.pair_live_photos);
        assert!(!captured.skip_incomplete_live_photos);
        assert!(captured.update_existing_to_live);
        assert_eq!(captured.album_name, "Holiday");
        assert!(!captured.album_auto_mode);

        // Settings changed for the next run must not change the captured run.
        prefs.proxy = String::new();
        prefs.saver = false;
        prefs.use_quota = false;
        prefs.upload_threads = 2;
        prefs.album_name = String::new();
        prefs.album_auto_mode = true;

        assert_eq!(captured.api.proxy, "http://proxy.example:8080");
        assert!(captured.api.saver);
        assert_eq!(captured.threads, 7);
        assert_eq!(captured.album_name, "Holiday");

        let next = base_options(&prefs);
        assert_eq!(next.api.proxy, "");
        assert!(!next.api.saver);
        assert!(!next.api.use_quota);
        assert_eq!(next.threads, 2);
        assert_eq!(next.album_name, "");
        assert!(next.album_auto_mode);
    }

    #[test]
    fn normalized_clamps_threads_and_clears_manual_album_in_auto_mode() {
        let mut opts = base_options(&Preferences::default());
        opts.threads = 0;
        opts.album_name = "Holiday".to_string();
        opts.album_auto_mode = true;
        let opts = opts.normalized();
        assert_eq!(opts.threads, 1);
        assert_eq!(opts.album_name, "");

        let mut opts = base_options(&Preferences::default());
        opts.threads = 4;
        opts.album_name = "Holiday".to_string();
        let opts = opts.normalized();
        assert_eq!(opts.threads, 4);
        assert_eq!(opts.album_name, "Holiday");
    }

    // ---- Preflight filtering over temp dirs (std only). ----

    struct Tree {
        root: PathBuf,
    }

    impl Tree {
        fn build() -> Tree {
            let root = std::env::temp_dir().join(format!(
                "outh-upload-test-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("sub")).expect("mkdir sub");
            fs::create_dir_all(root.join("skipme")).expect("mkdir skipme");
            let mut write = |rel: &str| {
                let path = root.join(rel);
                let mut f = fs::File::create(&path).expect("create fixture");
                f.write_all(b"x").expect("write fixture");
                path
            };
            write("a.jpg");
            write("b.txt");
            write("sub/c.png");
            write("sub/d.mp4");
            write("skipme/e.jpg");
            Tree { root }
        }

        fn names(&self, files: &[PathBuf]) -> Vec<String> {
            let mut names: Vec<String> = files
                .iter()
                .map(|p| {
                    p.strip_prefix(&self.root)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn opts_with(prefs: &Preferences) -> UploadOptions {
        UploadOptions::from_preferences(prefs).normalized()
    }

    #[test]
    fn preflight_non_recursive_drops_unsupported_and_dedupes() {
        let tree = Tree::build();
        let opts = opts_with(&Preferences::default());
        let a = tree.root.join("a.jpg");
        let files = filter_google_photos_files(
            &[tree.root.clone(), a.clone(), a.clone()],
            &opts,
        )
        .expect("filter");
        // Non-recursive: only top-level supported files; b.txt dropped;
        // a.jpg appears once despite being passed three ways.
        assert_eq!(tree.names(&files), vec!["a.jpg"]);
    }

    #[test]
    fn preflight_recursive_and_exclude_pattern() {
        let tree = Tree::build();
        let mut prefs = Preferences {
            recursive: true,
            ..Default::default()
        };
        let opts = opts_with(&prefs);
        let files = filter_google_photos_files(&[tree.root.clone()], &opts).expect("filter");
        assert_eq!(
            tree.names(&files),
            vec!["a.jpg", "skipme/e.jpg", "sub/c.png", "sub/d.mp4"]
        );

        // exclude_pattern matches a directory NAME exactly (Go behaviour).
        prefs.exclude_pattern = "skipme".to_string();
        let opts = opts_with(&prefs);
        let files = filter_google_photos_files(&[tree.root.clone()], &opts).expect("filter");
        assert_eq!(tree.names(&files), vec!["a.jpg", "sub/c.png", "sub/d.mp4"]);
    }

    #[test]
    fn preflight_disable_unsupported_filter_keeps_txt() {
        let tree = Tree::build();
        let prefs = Preferences {
            disable_unsupported_filter: true,
            ..Default::default()
        };
        let opts = opts_with(&prefs);
        let files = filter_google_photos_files(&[tree.root.clone()], &opts).expect("filter");
        assert_eq!(tree.names(&files), vec!["a.jpg", "b.txt"]);
    }

    #[test]
    fn preflight_missing_path_is_error() {
        let opts = opts_with(&Preferences::default());
        let missing = std::env::temp_dir().join("outh-upload-test-definitely-missing");
        let result = filter_google_photos_files(&[missing], &opts);
        assert!(result.is_err());
    }

    #[test]
    fn album_key_detection() {
        assert!(is_album_key("AF1QipExampleKey"));
        assert!(!is_album_key("AF1Qip")); // len must exceed 6, as in Go
        assert!(!is_album_key("Holiday"));
        assert!(!is_album_key(""));
    }

    #[test]
    fn supported_extensions_match_go_list() {
        assert!(is_supported(Path::new("x.JPG")));
        assert!(is_supported(Path::new("x.heic")));
        assert!(is_supported(Path::new("x.webm")));
        assert!(is_supported(Path::new(".jpg"))); // filepath.Ext parity
        assert!(!is_supported(Path::new("x.txt")));
        assert!(!is_supported(Path::new("noext")));
        assert!(!is_supported(Path::new("trailingdot.")));
    }
}
