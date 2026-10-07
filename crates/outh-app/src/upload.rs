//! Upload section: path list, album choice, start/cancel, aggregate and
//! per-worker progress, warnings and results.
//!
//! Threading choice (documented per the reactor model): the upload run is
//! NOT a reactor `spawn_background` task. `spawn_background` delivers
//! exactly one terminal message; an upload streams dozens of reporter
//! callbacks from core-owned worker threads for minutes or hours. So the
//! run lives on a dedicated `std::thread` (started in `start_upload`), the
//! reporter forwards every callback through a `ComponentCompletion`
//! (see `reporter.rs`), and cancellation uses core's own
//! `CancellationToken` — the Cancel button flips the shared `AtomicBool`
//! the core workers poll, exactly like Go's `UploadManager.Cancel`.

use std::path::PathBuf;
use std::sync::Arc;

use outh_core::api::{ApiOptions, PhotosClient};
use outh_core::config::Preferences;
use outh_core::credential::Credential;
use outh_core::types::{Stage, UploadReporter};
use outh_core::upload::{UploadManager, UploadOptions};
use windows_reactor::*;

use crate::reporter::SenderReporter;
use crate::{AlbumMode, Message, OuthApp, Section};

pub fn stage_label(stage: &Stage) -> &'static str {
    match stage {
        Stage::Hashing => "Hashing",
        Stage::Checking => "Checking",
        Stage::Uploading => "Uploading",
        Stage::Finalizing => "Finalizing",
        Stage::Completed => "Completed",
        Stage::Skipped => "Skipped",
        Stage::Error => "Error",
    }
}

pub(crate) fn file_label(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Builds the core run options from preferences + the session-only album
/// choice. This is the ONLY place `UploadOptions`/`ApiOptions` literals are
/// constructed; field names follow CONTRACT.md / Go's `upload_options.go`.
fn build_upload_options(
    preferences: &Preferences,
    album_mode: AlbumMode,
    album_name: &str,
) -> UploadOptions {
    UploadOptions {
        api: ApiOptions {
            proxy: preferences.proxy.clone(),
            saver: preferences.saver,
            use_quota: preferences.use_quota,
        },
        recursive: preferences.recursive,
        exclude_pattern: preferences.exclude_pattern.clone(),
        threads: preferences.upload_threads,
        force_upload: preferences.force_upload,
        delete_from_host: preferences.delete_from_host,
        // Go field name (upload_options.go: DisableUnsupportedFilesFilter),
        // fed from Preferences.disable_unsupported_filter (CONTRACT.md).
        disable_unsupported_filter: preferences.disable_unsupported_filter,
        set_date_from_filename: preferences.set_date_from_filename,
        pair_live_photos: preferences.pair_live_photos,
        skip_incomplete_live_photos: preferences.skip_incomplete_live_photos,
        update_existing_to_live: preferences.update_existing_to_live,
        ignore_apple_metadata: false,
        album_name: match album_mode {
            AlbumMode::Named => album_name.trim().to_string(),
            AlbumMode::None | AlbumMode::Auto => String::new(),
        },
        album_auto_mode: album_mode == AlbumMode::Auto,
    }
}

/// The blocking half of a run, on the dedicated upload thread.
///
/// INTEGRATION ASSUMPTIONS isolated here (CONTRACT.md gives
/// `UploadManager::new(photos_factory, reporter)` and
/// `run(inputs, opts, cancel) -> RunSummary` without fixing the factory
/// type): the factory is a closure called to build a `PhotosClient` from
/// the run's credential + `ApiOptions`, and the returned `RunSummary` is
/// ignored — all run outcomes reach the UI through the reporter.
fn run_blocking(
    credential_string: String,
    proxy: String,
    saver: bool,
    use_quota: bool,
    reporter: Arc<dyn UploadReporter>,
    inputs: Vec<PathBuf>,
    options: UploadOptions,
    cancel: outh_core::types::CancellationToken,
) -> String {
    let factory = move || {
        let credential = Credential::parse(&credential_string)?;
        PhotosClient::new(
            credential,
            ApiOptions {
                proxy: proxy.clone(),
                saver,
                use_quota,
            },
        )
    };
    let manager = UploadManager::new(factory, reporter);
    let _summary = manager.run(inputs, options, cancel);
    "Upload finished.".to_string()
}

/// Handles Message::StartUpload. Runs on the UI thread; only *starts* the
/// worker thread and flips state — all mutation of run state after this
/// happens in `update` via reporter messages.
pub fn start_upload(app: &mut OuthApp, context: &ComponentContext<OuthApp>) {
    if app.running {
        return;
    }
    let account = app
        .accounts
        .iter()
        .find(|account| app.active_email.as_deref() == Some(account.email.as_str()))
        .or_else(|| app.accounts.first())
        .cloned();
    let Some(account) = account else {
        app.upload_note =
            Some("Add an account first (Accounts section), then start the upload.".to_string());
        app.section = Section::Accounts;
        return;
    };
    if app.paths.is_empty() {
        app.upload_note = Some("Add at least one file or folder to upload.".to_string());
        return;
    }
    if app.album_mode == AlbumMode::Named && app.album_name.trim().is_empty() {
        app.upload_note =
            Some("Type an album name, or choose a different album option.".to_string());
        return;
    }

    app.results.clear();
    app.warnings.clear();
    app.workers.clear();
    app.total_files = 0;
    app.total_bytes = 0;
    app.album_status = None;
    app.cancel_requested = false;

    let cancel = outh_core::types::CancellationToken::new();
    app.cancel_token = Some(cancel.clone());
    app.running = true;
    app.upload_note = Some(format!("Uploading as {}…", account.email));

    let reporter: Arc<dyn UploadReporter> = Arc::new(SenderReporter::new(context.completion()));
    let completion = context.completion();
    let inputs = app.paths.clone();
    let options = build_upload_options(&app.prefs, app.album_mode, &app.album_name);
    let proxy = app.prefs.proxy.clone();
    let saver = app.prefs.saver;
    let use_quota = app.prefs.use_quota;
    let credential_string = account.credential.clone();

    std::thread::spawn(move || {
        let note = run_blocking(
            credential_string,
            proxy,
            saver,
            use_quota,
            reporter,
            inputs,
            options,
            cancel,
        );
        // Terminal signal, independent of the reporter's upload_stop: the
        // UI leaves the "running" state even if the run ended early.
        let _ = completion.complete(Message::UploadRunEnded(note));
    });
}

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let sender = context.sender();
    let mut children: Vec<View> = Vec::new();

    children.push(
        TextBlock::new()
            .text("Upload")
            .font_size(20.0)
            .into(),
    );
    children.push(
        TextBlock::new()
            .text(format!("Files and folders ({})", app.paths.len()))
            .into(),
    );

    if app.paths.is_empty() {
        children.push(
            TextBlock::new()
                .text("Nothing queued yet — add files or folders, or paste a path below.")
                .opacity(0.6)
                .into(),
        );
    }
    for (index, path) in app.paths.iter().enumerate() {
        let row_sender = sender.clone();
        children.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    TextBlock::new().text(path.display().to_string()),
                    Button::new()
                        .is_enabled(!app.running)
                        .on_click(move || {
                            _ = row_sender.send(Message::RemovePath(index));
                        })
                        .content("Remove"),
                ))
                .into(),
        );
    }

    children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                TextBox::new(app.path_draft.clone())
                    .placeholder_text("Paste a file or folder path, then Add")
                    .is_enabled(!app.running)
                    .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                        Message::PathDraftChanged(value.to_string())
                    })),
                Button::new()
                    .is_enabled(!app.running)
                    .on_click(context.callback(|()| Message::AddPathDraft))
                    .content("Add path"),
                Button::new()
                    .is_enabled(!app.running)
                    .on_click(context.callback(|()| Message::AddFiles))
                    .content("Add files…"),
                Button::new()
                    .is_enabled(!app.running)
                    .on_click(context.callback(|()| Message::AddFolders))
                    .content("Add folder…"),
                Button::new()
                    .is_enabled(!app.running && !app.paths.is_empty())
                    .on_click(context.callback(|()| Message::ClearPaths))
                    .content("Clear"),
            ))
            .into(),
    );

    children.push(
        RadioButtons::new()
            .items_source(["No album", "Named album", "Auto — one album per folder"])
            .selected_index(Some(app.album_mode.index()))
            .on_selection_changed(
                context.callback(|index: Option<usize>| Message::AlbumModeChanged(index)),
            )
            .header("Album")
            .into(),
    );
    if app.album_mode == AlbumMode::Named {
        children.push(
            TextBox::new(app.album_name.clone())
                .placeholder_text("Album name (or an existing album key)")
                .is_enabled(!app.running)
                .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::AlbumNameChanged(value.to_string())
                }))
                .into(),
        );
    }

    children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                Button::new()
                    .is_enabled(!app.running && !app.paths.is_empty())
                    .on_click(context.callback(|()| Message::StartUpload))
                    .content(if app.running { "Uploading…" } else { "Start upload" }),
                Button::new()
                    .is_enabled(app.running)
                    .on_click(context.callback(|()| Message::CancelUpload))
                    .content("Cancel"),
            ))
            .into(),
    );

    if let Some(note) = &app.upload_note {
        children.push(TextBlock::new().text(note.clone()).into());
    }

    // Aggregate progress: bytes are summed from the per-worker rows (the
    // same approximation gotohp's GUI uses); file counts come from the
    // authoritative file_result stream.
    let uploaded_bytes: u64 = app.workers.iter().map(|row| row.bytes_uploaded).sum();
    if app.total_bytes > 0 || app.running {
        children.push(
            ProgressBar::new()
                .minimum(0.0)
                .maximum(app.total_bytes.max(1) as f64)
                .value(uploaded_bytes.min(app.total_bytes) as f64)
                .is_indeterminate(app.running && app.total_bytes == 0)
                .into(),
        );
    }
    let uploaded = app
        .results
        .iter()
        .filter(|row| row.outcome == "Uploaded")
        .count();
    let skipped = app
        .results
        .iter()
        .filter(|row| row.outcome.starts_with("Skipped"))
        .count();
    let failed = app
        .results
        .iter()
        .filter(|row| row.outcome == "Failed")
        .count();
    children.push(
        TextBlock::new()
            .text(format!(
                "{} file(s) queued · {} uploaded · {} skipped · {} failed",
                app.total_files.max(app.results.len()),
                uploaded,
                skipped,
                failed
            ))
            .into(),
    );

    for row in &app.workers {
        let mut line = format!(
            "Worker {} — {} — {}",
            row.worker + 1,
            row.stage,
            row.file
        );
        if row.bytes_total > 0 {
            line.push_str(&format!(
                " ({} / {} bytes)",
                row.bytes_uploaded, row.bytes_total
            ));
        }
        if row.attempt > 1 {
            line.push_str(&format!(" · attempt {}", row.attempt));
        }
        children.push(TextBlock::new().text(line).opacity(0.8).into());
    }

    if let Some(status) = &app.album_status {
        children.push(TextBlock::new().text(status.clone()).into());
    }

    if !app.warnings.is_empty() {
        children.push(TextBlock::new().text("Warnings").font_size(16.0).into());
        for warning in app.warnings.iter().rev().take(10) {
            children.push(
                TextBlock::new()
                    .text(warning.clone())
                    .text_wrapping(TextWrapping::Wrap)
                    .opacity(0.8)
                    .into(),
            );
        }
    }

    if !app.results.is_empty() {
        children.push(
            TextBlock::new()
                .text(format!("Results ({})", app.results.len()))
                .font_size(16.0)
                .into(),
        );
        if app.results.len() > 200 {
            children.push(
                TextBlock::new()
                    .text(format!(
                        "Showing the latest 200 of {} results.",
                        app.results.len()
                    ))
                    .opacity(0.6)
                    .into(),
            );
        }
        // Skipped counts as success (the file is already in the library),
        // mirroring gotohp's model.
        for row in app.results.iter().rev().take(200) {
            let mut line = format!("{} — {}", row.file, row.outcome);
            if let Some(message) = &row.message {
                line.push_str(&format!(": {message}"));
            }
            children.push(
                TextBlock::new()
                    .text(line)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
    }

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(24.0, 24.0, 24.0, 24.0))
                .content(StackPanel::new().spacing(10.0).children(children)),
        )
        .into()
}
