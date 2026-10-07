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
use outh_core::upload::{RunSummary, UploadManager, UploadOptions};
use windows_reactor::*;

use crate::reporter::SenderReporter;
use crate::theme;
use crate::{AlbumMode, Message, Note, NoteSeverity, OuthApp, RunEnded, Section};

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

/// The blocking half of a run, on the dedicated upload thread. Returns
/// core's `RunSummary` (spec §3.5): the thread converts it to the app's
/// `RunEnded` and delivers it as `Message::UploadRunEnded`, so terminal
/// counts come from the authoritative summary — streamed reporter
/// outcomes remain the per-file detail. (The factory is a closure called
/// to build a `PhotosClient` from the run's credential + `ApiOptions`.)
fn run_blocking(
    credential_string: String,
    reporter: Arc<dyn UploadReporter>,
    inputs: Vec<PathBuf>,
    options: UploadOptions,
    cancel: outh_core::types::CancellationToken,
) -> RunSummary {
    // The manager passes each run's ApiOptions (built from preferences by
    // build_upload_options) to the factory; the factory only adds the
    // run's credential.
    let factory: outh_core::upload::PhotosFactory = Arc::new(move |api: &ApiOptions| {
        let credential = Credential::parse(&credential_string)?;
        PhotosClient::new(credential, api.clone())
    });
    let manager = UploadManager::new(factory, reporter);
    manager.run(inputs, options, cancel)
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
        app.upload_note = Some(Note::warning(
            "Add an account first (Accounts section), then start the upload.",
        ));
        app.section = Section::Accounts;
        return;
    };
    if app.paths.is_empty() {
        app.upload_note = Some(Note::warning("Add at least one file or folder to upload."));
        return;
    }
    if app.album_mode == AlbumMode::Named && app.album_name.trim().is_empty() {
        app.upload_note = Some(Note::warning(
            "Type an album name, or choose a different album option.",
        ));
        return;
    }

    app.results.clear();
    app.warnings.clear();
    app.workers.clear();
    app.total_files = 0;
    app.total_bytes = 0;
    app.album_status = None;
    app.run_summary = None;
    app.cancel_requested = false;

    let cancel = outh_core::types::CancellationToken::new();
    app.cancel_token = Some(cancel.clone());
    app.running = true;
    app.upload_note = Some(Note::info(format!("Uploading as {}…", account.email)));

    let reporter: Arc<dyn UploadReporter> = Arc::new(SenderReporter::new(context.completion()));
    let completion = context.completion();
    let inputs = app.paths.clone();
    let options = build_upload_options(&app.prefs, app.album_mode, &app.album_name);
    let credential_string = account.credential.clone();

    std::thread::spawn(move || {
        let summary = run_blocking(credential_string, reporter, inputs, options, cancel);
        // Terminal signal, independent of the reporter's upload_stop: the
        // UI leaves the "running" state even if the run ended early.
        let _ = completion.complete(Message::UploadRunEnded(RunEnded::from(summary)));
    });
}

pub fn view(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let mut children: Vec<View> = Vec::new();

    children.push(theme::page_title("Upload"));
    // The page's feedback lives directly under the title (spec §2) —
    // one consistent home on every page.
    if let Some(note) = &app.upload_note {
        children.push(theme::note_bar(
            note,
            context.message(Message::DismissUploadNote),
        ));
    }

    if app.accounts.is_empty() {
        // Readiness (spec §4.1): with no account there is nothing to
        // upload with, so the empty state replaces the whole queue UI
        // (album and Start stay hidden) until an account exists.
        children.push(theme::empty_state(
            Symbol::People,
            "Connect an account first",
            "Outh uploads through your Google account. Connect one, then \
             come back here to add files and start uploading.",
            Button::new()
                .style(ButtonStyle::Accent)
                .on_click(context.callback(|()| {
                    Message::NavigateTag(Some("accounts".to_string()))
                }))
                .content("Go to Accounts")
                .into(),
        ));
    } else {
        children.push(queue_section(app, context));
        children.push(album_area(app, context));
        children.push(actions_row(app, context));
        if app.running {
            children.push(progress_area(app));
        } else if let Some(summary) = app.run_summary {
            // The completion panel replaces the progress area once a
            // run has ended (spec §4.6).
            children.push(completion_panel(app, context, summary));
        }
        if !app.warnings.is_empty() {
            children.push(warnings_area(app));
        }
        if !app.results.is_empty() {
            children.push(results_area(app));
        }
    }

    ScrollViewer::new()
        .content(
            Border::new()
                .padding(Thickness::new(
                    theme::PAGE_PADDING_X,
                    theme::PAGE_PADDING_TOP,
                    theme::PAGE_PADDING_X,
                    theme::SPACE_XL,
                ))
                .content(
                    StackPanel::new()
                        .spacing(theme::SPACE_XL)
                        .max_width(theme::CONTENT_MAX_WIDTH)
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .children(children),
                ),
        )
        .into()
}

/// The queue as a worklist (spec §4.2/§4.3): strong count header, one
/// card per queued path, the add row, and the drag hint — the whole
/// section is the page's drop target.
fn queue_section(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let mut children: Vec<View> = Vec::new();
    children.push(theme::strong(format!("Queue ({})", app.paths.len())));
    if app.paths.is_empty() {
        children.push(theme::caption(
            "Nothing queued yet — add files or folders below, or drag them here.",
        ));
    }
    for (index, path) in app.paths.iter().enumerate() {
        children.push(queue_row(app, context, index, path));
    }
    children.push(add_row(app, context));
    children.push(theme::caption("or drag files and folders here"));

    let panel = StackPanel::new()
        .spacing(theme::SPACE_M)
        .children(children);
    // Drag-and-drop (I-01), wired exactly as the reactor drag-drop
    // sample: a storage-items policy + on_drop. Attached only while
    // idle — mid-run the queue is locked (Remove/Add are disabled too),
    // so the drop target leaves with them.
    let mut area = Border::new().content(panel);
    if !app.running {
        area = area
            .drop_policy(
                DragDropPolicy::new().storage_items(
                    DragDropAction::new(DragDropOperation::Copy)
                        .caption("Add to the upload queue"),
                ),
            )
            .on_drop(context.callback(|data: DroppedData| match data {
                DroppedData::StorageItems(items) => Message::FilesDropped(
                    items
                        .into_iter()
                        .map(|item| PathBuf::from(item.path))
                        .collect(),
                ),
                _ => Message::FilesDropped(Vec::new()),
            }));
    }
    area.into()
}

/// One queued path as a card row (spec §4.2): kind icon (folder vs
/// file), file name Strong with the full path as a Caption beneath,
/// and a Subtle Remove action.
fn queue_row(
    app: &OuthApp,
    context: &mut ViewContext<OuthApp>,
    index: usize,
    path: &PathBuf,
) -> View {
    let name = file_label(path);
    let symbol = if path.is_dir() {
        Symbol::Folder
    } else {
        Symbol::Pictures
    };
    let row = Grid::new()
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .column_spacing(theme::SPACE_M)
        .children(vec![
            SymbolIcon::new()
                .symbol(symbol)
                .grid_column(0)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
            StackPanel::new()
                .grid_column(1)
                .spacing(theme::SPACE_XS)
                .children(vec![
                    theme::strong(name.clone()),
                    theme::caption(path.display().to_string()),
                ])
                .into(),
            Button::new()
                .grid_column(2)
                .vertical_alignment(VerticalAlignment::Center)
                .style(ButtonStyle::Subtle)
                .is_enabled(!app.running)
                .automation_name(format!("Remove {name} from the queue"))
                .on_click(context.callback(move |()| Message::RemovePath(index)))
                .content("Remove")
                .into(),
        ]);
    theme::card(vec![row.into()])
}

/// The add row (spec §4.3): a star column for the path box so the
/// buttons can never be clipped (I-02) — the star shrinks first.
/// Enter in the box adds the path (I-05): TextBox has no key events in
/// this stack, so a wrapping Border routes preview key-down, the
/// pattern AUDIT_3 verified.
fn add_row(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    let key_sender = context.sender();
    let enter_to_add = RoutedCallback::new(move |info: KeyEventInfo| {
        if info.key == VirtualKey::ENTER {
            let _ = key_sender.send(Message::AddPathDraft);
            true
        } else {
            false
        }
    });
    let text_cell = Border::new()
        .grid_column(0)
        .on_preview_key_down(enter_to_add)
        .content(
            TextBox::new(app.path_draft.clone())
                .header("Add by path")
                .placeholder_text("Paste a file or folder path, then Add")
                .is_enabled(!app.running)
                .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::PathDraftChanged(value.to_string())
                })),
        );
    Grid::new()
        .columns([
            GridLength::STAR,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
            GridLength::Auto,
        ])
        .column_spacing(theme::SPACE_S)
        .children(vec![
            text_cell.into(),
            Button::new()
                .grid_column(1)
                .vertical_alignment(VerticalAlignment::Bottom)
                .is_enabled(!app.running)
                .on_click(context.callback(|()| Message::AddPathDraft))
                .content("Add")
                .into(),
            Button::new()
                .grid_column(2)
                .vertical_alignment(VerticalAlignment::Bottom)
                .is_enabled(!app.running)
                .on_click(context.callback(|()| Message::AddFiles))
                .content("Add files…")
                .into(),
            Button::new()
                .grid_column(3)
                .vertical_alignment(VerticalAlignment::Bottom)
                .is_enabled(!app.running)
                .on_click(context.callback(|()| Message::AddFolders))
                .content("Add folder…")
                .into(),
            Button::new()
                .grid_column(4)
                .vertical_alignment(VerticalAlignment::Bottom)
                .style(ButtonStyle::Subtle)
                .is_enabled(!app.running && !app.paths.is_empty())
                // ClearPaths routes to the global confirmation dialog
                // in main.rs (spec §2) — same as RequestClearQueue.
                .on_click(context.callback(|()| Message::ClearPaths))
                .content("Clear")
                .into(),
        ])
        .into()
}

/// Album choice (spec §4.4). `RadioButtons` has no `is_enabled` in
/// this stack (schema gap), so while a run is active the area renders
/// as a static summary line instead of live radios that would silently
/// not apply (I-17).
fn album_area(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    if app.running {
        let line = match app.album_mode {
            AlbumMode::None => {
                "Album: none — files go straight to your library".to_string()
            }
            AlbumMode::Named => format!("Album: {}", app.album_name),
            AlbumMode::Auto => "Album: one album per source folder".to_string(),
        };
        return theme::body(line);
    }
    let mut children: Vec<View> = Vec::new();
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
                .header("Album name")
                .placeholder_text("Album name (or an existing album key)")
                .on_text_changed(context.callback(|value: std::rc::Rc<str>| {
                    Message::AlbumNameChanged(value.to_string())
                }))
                .into(),
        );
    }
    StackPanel::new()
        .spacing(theme::SPACE_S)
        .children(children)
        .into()
}

/// The account a run would use — the same selection `start_upload`
/// makes (active account, else the first).
fn active_email(app: &OuthApp) -> Option<String> {
    app.accounts
        .iter()
        .find(|account| app.active_email.as_deref() == Some(account.email.as_str()))
        .or_else(|| app.accounts.first())
        .map(|account| account.email.clone())
}

/// Start / Cancel actions (spec §4.4): Start is the page's one Accent
/// button, labelled with the account it will upload as; while running
/// it gives way to Cancel, and to a disabled "Cancelling…" once a
/// cancel is in flight.
fn actions_row(app: &OuthApp, context: &mut ViewContext<OuthApp>) -> View {
    if app.running {
        return if app.cancel_requested {
            Button::new()
                .is_enabled(false)
                .content("Cancelling…")
                .into()
        } else {
            Button::new()
                .on_click(context.callback(|()| Message::CancelUpload))
                .content("Cancel")
                .into()
        };
    }
    let mut row: Vec<View> = Vec::new();
    row.push(
        Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(!app.paths.is_empty())
            .on_click(context.callback(|()| Message::StartUpload))
            .content("Start upload")
            .into(),
    );
    if let Some(email) = active_email(app) {
        row.push(
            Border::new()
                .vertical_alignment(VerticalAlignment::Center)
                .content(theme::caption(format!("Uploading as {email}")))
                .into(),
        );
    }
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(theme::SPACE_M)
        .children(row)
        .into()
}

/// The live run dashboard (spec §4.5). Truthful progress only: the
/// bar is the finished-files fraction (results are the authoritative
/// stream) and stays indeterminate while preflight has not produced a
/// total yet; bytes are the summed per-worker counters, shown as a
/// text twin, never blended into the bar.
fn progress_area(app: &OuthApp) -> View {
    let finished = app.results.len();
    let uploaded_bytes: u64 = app.workers.iter().map(|row| row.bytes_uploaded).sum();
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

    let mut children: Vec<View> = Vec::new();
    let mut bar = ProgressBar::new()
        .minimum(0.0)
        .automation_name("Upload progress");
    if app.total_files > 0 {
        bar = bar
            .maximum(app.total_files as f64)
            .value(finished.min(app.total_files) as f64)
            .is_indeterminate(false);
    } else {
        bar = bar.is_indeterminate(true);
    }
    children.push(bar.into());

    let counts = if app.total_files > 0 {
        let mut line = format!(
            "{finished} of {} files · {uploaded} uploaded · {skipped} skipped · {failed} failed",
            app.total_files
        );
        if app.total_bytes > 0 {
            line.push_str(&format!(
                " · {} of {}",
                theme::fmt_bytes(uploaded_bytes),
                theme::fmt_bytes(app.total_bytes)
            ));
        }
        line
    } else {
        "Scanning files…".to_string()
    };
    children.push(theme::body(counts));

    for row in &app.workers {
        let mut line = format!(
            "Worker {} · {} · {}",
            row.worker + 1,
            row.stage,
            row.file
        );
        if row.bytes_total > 0 {
            line.push_str(&format!(
                " · {} / {}",
                theme::fmt_bytes(row.bytes_uploaded),
                theme::fmt_bytes(row.bytes_total)
            ));
        }
        // Core currently reports attempt 0 until its first retry, so
        // an attempt is shown only when it is real (> 1) — never
        // fabricated (§4.5).
        if row.attempt > 1 {
            line.push_str(&format!(" · attempt {}", row.attempt));
        }
        children.push(theme::secondary(line));
    }

    if let Some(status) = &app.album_status {
        children.push(theme::caption(status.clone()));
    }

    StackPanel::new()
        .spacing(theme::SPACE_S)
        .children(children)
        .into()
}

/// The completion panel (spec §4.6): a card rendered from the run's
/// terminal summary once it has ended — headline, counts, album
/// outcome, and the three follow-up actions.
fn completion_panel(
    app: &OuthApp,
    context: &mut ViewContext<OuthApp>,
    summary: RunEnded,
) -> View {
    let mut children: Vec<View> = Vec::new();
    children.push(theme::section_title(if summary.cancelled {
        "Run cancelled"
    } else {
        "Run complete"
    }));
    children.push(theme::body(format!(
        "{} uploaded · {} skipped · {} failed",
        summary.uploaded, summary.skipped, summary.failed
    )));
    if let Some(status) = &app.album_status {
        children.push(theme::secondary(status.clone()));
    }
    let mut actions: Vec<View> = Vec::new();
    if summary.failed > 0 {
        actions.push(
            Button::new()
                .style(ButtonStyle::Accent)
                .on_click(context.callback(|()| Message::RetryFailed))
                .content(format!("Retry failed ({})", summary.failed))
                .into(),
        );
    }
    actions.push(
        Button::new()
            .style(ButtonStyle::Subtle)
            .on_click(context.callback(|()| Message::RemoveCompletedFromQueue))
            .content("Remove completed from queue")
            .into(),
    );
    actions.push(
        HyperlinkButton::new()
            .on_click(context.callback(|()| Message::OpenGooglePhotos))
            .content("Open Google Photos")
            .into(),
    );
    children.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(theme::SPACE_S)
            .children(actions)
            .into(),
    );
    theme::card(children)
}

/// Preflight warnings (spec §4.7): ONE Warning summary bar for the
/// whole set — a bar per warning would flood the page — with the full
/// list in a collapsed Expander.
fn warnings_area(app: &OuthApp) -> View {
    let count = app.warnings.len();
    let mut children: Vec<View> = Vec::new();
    children.push(theme::info_bar(
        NoteSeverity::Warning,
        None,
        &format!(
            "{count} preflight warning{}",
            if count == 1 { "" } else { "s" }
        ),
        None,
    ));
    children.push(
        Expander::new()
            .header(theme::body("Warning details"))
            .content(
                StackPanel::new().spacing(theme::SPACE_XS).children(
                    app.warnings
                        .iter()
                        .map(|warning| theme::caption(warning.clone()))
                        .collect::<Vec<View>>(),
                ),
            )
            .into(),
    );
    StackPanel::new()
        .spacing(theme::SPACE_S)
        .children(children)
        .into()
}

/// Finished files (spec §4.7): name Strong + outcome Caption; a
/// failed row's outcome word carries the SystemCritical brush (the
/// word itself stays, so meaning is never color-only). Latest 200,
/// with the cap stated.
fn results_area(app: &OuthApp) -> View {
    let mut children: Vec<View> = Vec::new();
    children.push(theme::strong(format!("Results ({})", app.results.len())));
    if app.results.len() > 200 {
        children.push(theme::caption(format!(
            "Showing the latest 200 of {} results.",
            app.results.len()
        )));
    }
    // Skipped counts as success (the file is already in the library),
    // mirroring gotohp's model.
    for row in app.results.iter().rev().take(200) {
        let outcome: View = if row.outcome == "Failed" {
            TextBlock::new()
                .text(row.outcome.clone())
                .font_size(theme::TYPE_CAPTION)
                .font_weight(FontWeight::NORMAL)
                .foreground(ThemeBrush::SystemCritical)
                .into()
        } else {
            theme::caption(row.outcome.clone())
        };
        let mut lines: Vec<View> = vec![
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(theme::SPACE_S)
                .children(vec![theme::strong(row.file.clone()), outcome])
                .into(),
        ];
        if let Some(message) = &row.message {
            lines.push(theme::caption(message.clone()));
        }
        children.push(
            StackPanel::new()
                .spacing(theme::SPACE_XS)
                .children(lines)
                .into(),
        );
    }
    StackPanel::new()
        .spacing(theme::SPACE_S)
        .children(children)
        .into()
}
