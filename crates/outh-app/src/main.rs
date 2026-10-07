#![windows_subsystem = "windows"]

//! Outh — Google Photos uploader for Windows, on windows-reactor (WinUI 3).
//!
//! A single root component, `OuthApp`, owns all UI state (the reactor
//! model: state mutates only in `update`, callbacks only enqueue
//! messages). Sections are plain view functions in their own modules:
//! `upload.rs`, `accounts.rs`, `settings.rs`. Cross-thread work reports
//! back as messages — uploads via the `UploadReporter` bridge in
//! `reporter.rs`, the sign-in exchange via `spawn_background` in
//! `accounts.rs`.

mod accounts;
mod protector;
mod reporter;
mod settings;
mod store;
mod theme;
mod upload;

use std::path::PathBuf;
use std::rc::Rc;

use outh_core::config::{Account, ConfigService, Preferences};
use outh_core::credential::Credential;
use outh_core::types::{AlbumStatus, FileResult, Outcome, PreflightWarning, ThreadStatus};
use windows_pickers::{FolderPicker, OpenFilePicker};
use windows_reactor::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Upload,
    Accounts,
    Settings,
}

/// Severity for a user-facing note (spec §2). Every page renders its
/// note through `theme::info_bar`, so severity is always visible —
/// replacing the old undifferentiated `Option<String>` notes (F-38).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteSeverity {
    Info,
    Success,
    Warning,
    Error,
}

/// A page-level feedback message with a severity.
#[derive(Clone, Debug)]
pub struct Note {
    pub severity: NoteSeverity,
    pub text: String,
}

impl Note {
    pub fn info(text: impl Into<String>) -> Note {
        Note {
            severity: NoteSeverity::Info,
            text: text.into(),
        }
    }

    pub fn success(text: impl Into<String>) -> Note {
        Note {
            severity: NoteSeverity::Success,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Note {
        Note {
            severity: NoteSeverity::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Note {
        Note {
            severity: NoteSeverity::Error,
            text: text.into(),
        }
    }
}

/// A pending global confirmation (spec §2). Exactly one declarative
/// ContentDialog in the root view renders this; the destructive action
/// runs only when the dialog resolves with Primary (`ConfirmResolved`).
#[derive(Clone, Debug)]
pub enum Confirm {
    RemoveAccount(String),
    ClearQueue,
    DeleteOriginals,
}

/// Terminal counts of one upload run, converted from core's `RunSummary`
/// at the upload-thread boundary (spec §3.5). Stored on the app as
/// `run_summary` so the completion panel can render from it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunEnded {
    pub total_items: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
}

impl From<outh_core::upload::RunSummary> for RunEnded {
    fn from(summary: outh_core::upload::RunSummary) -> Self {
        RunEnded {
            total_items: summary.total_items,
            uploaded: summary.uploaded,
            skipped: summary.skipped,
            failed: summary.failed,
            cancelled: summary.cancelled,
        }
    }
}

/// Album choice in the Upload section (session-only, as in Go).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AlbumMode {
    None,
    Named,
    Auto,
}

impl AlbumMode {
    fn index(self) -> usize {
        match self {
            AlbumMode::None => 0,
            AlbumMode::Named => 1,
            AlbumMode::Auto => 2,
        }
    }

    fn from_index(index: Option<usize>) -> AlbumMode {
        match index {
            Some(1) => AlbumMode::Named,
            Some(2) => AlbumMode::Auto,
            _ => AlbumMode::None,
        }
    }
}

/// Identifies one boolean `Preferences` field, so all settings toggles
/// share a single message + handler.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PrefBool {
    UseQuota,
    Saver,
    Recursive,
    ForceUpload,
    PairLivePhotos,
    SkipIncompleteLivePhotos,
    UpdateExistingToLive,
    DeleteFromHost,
    DisableUnsupportedFilter,
    SetDateFromFilename,
}

impl PrefBool {
    fn apply(self, prefs: &mut Preferences, value: bool) {
        match self {
            PrefBool::UseQuota => prefs.use_quota = value,
            PrefBool::Saver => prefs.saver = value,
            PrefBool::Recursive => prefs.recursive = value,
            PrefBool::ForceUpload => prefs.force_upload = value,
            PrefBool::PairLivePhotos => prefs.pair_live_photos = value,
            PrefBool::SkipIncompleteLivePhotos => prefs.skip_incomplete_live_photos = value,
            PrefBool::UpdateExistingToLive => prefs.update_existing_to_live = value,
            PrefBool::DeleteFromHost => prefs.delete_from_host = value,
            PrefBool::DisableUnsupportedFilter => prefs.disable_unsupported_filter = value,
            PrefBool::SetDateFromFilename => prefs.set_date_from_filename = value,
        }
    }
}

pub enum Message {
    // Navigation (payload: the NavigationView item tag)
    NavigateTag(Option<String>),

    // Feedback & confirmations (spec §2)
    DismissUploadNote,
    DismissAccountNote,
    DismissSettingsNote,
    RequestRemoveAccount(String),
    RequestClearQueue,
    ConfirmResolved(bool),

    // Upload section
    PathDraftChanged(String),
    AddPathDraft,
    AddFiles,
    AddFolders,
    PathsPicked(Vec<PathBuf>),
    FilesDropped(Vec<PathBuf>),
    PickerFailed(String),
    RemovePath(usize),
    ClearPaths,
    AlbumModeChanged(Option<usize>),
    AlbumNameChanged(String),
    StartUpload,
    CancelUpload,
    RetryFailed,
    RemoveCompletedFromQueue,
    OpenGooglePhotos,

    // Upload run — sent by the reporter bridge / upload thread
    UploadStarted(usize),
    UploadStopped,
    UploadTotalBytes(u64),
    UploadTotalBytesDelta(i64),
    UploadWarning(PreflightWarning),
    UploadThreadStatus(ThreadStatus),
    UploadFileResult(FileResult),
    UploadAlbumProgress(AlbumStatus),
    UploadAlbumComplete(AlbumStatus),
    UploadAlbumError(String, String),
    UploadRunEnded(RunEnded),

    // Accounts section
    OAuthTokenChanged(String),
    RawCredentialChanged(String),
    OpenSignInPage,
    ConnectAccount,
    AccountConnected(Result<(String, String, bool), String>),
    ImportRawCredential,
    SetActiveAccount(String),
    RemoveAccount(String),

    // Settings section
    PrefBoolChanged(PrefBool, bool),
    PrefThreadsChanged(Option<f64>),
    PrefProxyChanged(String),
    PrefExcludePatternChanged(String),
}

/// One worker's latest status, for the per-worker rows.
pub struct WorkerRow {
    pub worker: usize,
    pub stage: String,
    pub file: String,
    pub bytes_uploaded: u64,
    pub bytes_total: u64,
    pub attempt: u32,
}

/// One finished file, for the results list. `file` is the display label
/// (name only); `path` is the full path, kept so Retry failed can
/// re-queue exactly this file (spec §3.6).
pub struct ResultRow {
    pub file: String,
    pub path: PathBuf,
    pub outcome: String,
    pub message: Option<String>,
}

pub struct OuthApp {
    pub section: Section,

    // Config (loaded at startup; `store` is None only if loading failed,
    // in which case the app runs on defaults and shows `store_error`).
    pub store: Option<ConfigService>,
    pub store_error: Option<String>,
    pub accounts: Vec<Account>,
    pub active_email: Option<String>,
    pub prefs: Preferences,

    // Upload section state
    pub paths: Vec<PathBuf>,
    pub path_draft: String,
    pub album_mode: AlbumMode,
    pub album_name: String,
    pub running: bool,
    pub cancel_requested: bool,
    pub cancel_token: Option<outh_core::types::CancellationToken>,
    pub total_files: usize,
    pub total_bytes: u64,
    pub workers: Vec<WorkerRow>,
    pub results: Vec<ResultRow>,
    pub warnings: Vec<String>,
    pub album_status: Option<String>,
    pub upload_note: Option<Note>,
    /// Terminal summary of the last finished run (completion panel data).
    pub run_summary: Option<RunEnded>,

    // Accounts section state
    pub oauth_token: String,
    pub raw_credential: String,
    pub auth_busy: bool,
    pub account_note: Option<Note>,

    // Settings section state
    pub settings_note: Option<Note>,

    // Global confirmation awaiting the root ContentDialog (spec §2)
    pub confirm: Option<Confirm>,
}

impl OuthApp {
    /// Re-reads accounts / active account / preferences from the store
    /// after a mutation (the store persists on every write).
    fn refresh_from_store(&mut self) {
        if let Some(service) = &self.store {
            let config = service.config().clone();
            self.accounts = config.accounts;
            self.active_email = config.active_email;
            self.prefs = config.preferences;
        }
    }

    fn persist_preferences(&mut self) {
        if let Some(service) = &mut self.store {
            match store::save_preferences(service, &self.prefs) {
                Ok(()) => self.settings_note = None,
                Err(error) => {
                    self.settings_note =
                        Some(Note::error(format!("Could not save settings: {error}")));
                }
            }
        }
    }

    fn add_account(&mut self, account: Account) {
        let email = account.email.clone();
        match &mut self.store {
            Some(service) => match store::add_account(service, account) {
                Ok(()) => {
                    // A newly added account becomes the active one, as in
                    // gotohp (the GUI selects the account it just added).
                    let _ = store::set_active(service, &email);
                    self.refresh_from_store();
                    self.account_note = Some(Note::success(format!("Added account {email}.")));
                }
                Err(error) => {
                    self.account_note =
                        Some(Note::error(format!("Could not save the account: {error}")));
                }
            },
            None => {
                self.account_note = Some(Note::error(
                    "Config could not be loaded, so the account was not saved.",
                ));
            }
        }
    }

    /// Applies an account removal (the destructive half of the
    /// remove-account confirmation, spec §2). Only called from
    /// `ConfirmResolved(true)` or the legacy `RemoveAccount` arm.
    fn remove_account_now(&mut self, email: &str) {
        match &mut self.store {
            Some(service) => match store::remove_account(service, email) {
                Ok(()) => {
                    self.refresh_from_store();
                    self.account_note =
                        Some(Note::success(format!("Removed account {email}.")));
                }
                Err(error) => {
                    self.account_note =
                        Some(Note::error(format!("Could not remove account: {error}")));
                }
            },
            None => {
                self.account_note = Some(Note::error(
                    "Config could not be loaded; accounts are unavailable.",
                ));
            }
        }
    }
}

impl Component for OuthApp {
    type Message = Message;
    type Input = ();

    fn create(_input: &(), _context: &ComponentContext<Self>) -> Self {
        let (store, store_error) = store::load();
        let mut app = Self {
            section: Section::Upload,
            store,
            store_error,
            accounts: Vec::new(),
            active_email: None,
            prefs: Preferences::default(),
            paths: Vec::new(),
            path_draft: String::new(),
            album_mode: AlbumMode::None,
            album_name: String::new(),
            running: false,
            cancel_requested: false,
            cancel_token: None,
            total_files: 0,
            total_bytes: 0,
            workers: Vec::new(),
            results: Vec::new(),
            warnings: Vec::new(),
            album_status: None,
            upload_note: None,
            run_summary: None,
            oauth_token: String::new(),
            raw_credential: String::new(),
            auth_busy: false,
            account_note: None,
            settings_note: None,
            confirm: None,
        };
        app.refresh_from_store();
        // Album choice is session state, initialised from the loaded
        // preferences (Go keeps it out of the persisted file; the Rust
        // Preferences struct carries the fields, so honour them at start).
        if app.prefs.album_auto_mode {
            app.album_mode = AlbumMode::Auto;
        } else if !app.prefs.album_name.trim().is_empty() {
            app.album_mode = AlbumMode::Named;
            app.album_name = app.prefs.album_name.clone();
        }
        // First-run routing (F-01): with no account there is nothing to
        // upload with, so land on Accounts where the sign-in card is the
        // hero. The Upload page's empty state covers later states.
        if app.accounts.is_empty() {
            app.section = Section::Accounts;
        }
        app
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::NavigateTag(tag) => {
                match tag.as_deref() {
                    Some("upload") => self.section = Section::Upload,
                    Some("accounts") => self.section = Section::Accounts,
                    Some("settings") => self.section = Section::Settings,
                    _ => {}
                }
            }

            // ---- Feedback & confirmations (spec §2) ----
            Message::DismissUploadNote => self.upload_note = None,
            Message::DismissAccountNote => self.account_note = None,
            Message::DismissSettingsNote => self.settings_note = None,
            Message::RequestRemoveAccount(email) => {
                self.confirm = Some(Confirm::RemoveAccount(email));
            }
            Message::RequestClearQueue => {
                self.confirm = Some(Confirm::ClearQueue);
            }
            Message::ConfirmResolved(agreed) => {
                // Take first so a re-render never shows a stale dialog.
                if let Some(confirm) = self.confirm.take() {
                    if agreed {
                        match confirm {
                            Confirm::RemoveAccount(email) => {
                                self.remove_account_now(&email);
                            }
                            Confirm::ClearQueue => self.paths.clear(),
                            Confirm::DeleteOriginals => {
                                self.prefs.delete_from_host = true;
                                self.persist_preferences();
                            }
                        }
                    }
                }
            }
            Message::FilesDropped(paths) => {
                // Dropped paths merge exactly like picker results.
                for path in paths {
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                }
            }
            Message::RetryFailed => {
                // Re-queue exactly the failed files (their full paths
                // from the results) and start a fresh run through the
                // same path as StartUpload (spec §4.6).
                let failed: Vec<PathBuf> = self
                    .results
                    .iter()
                    .filter(|row| row.outcome == "Failed")
                    .map(|row| row.path.clone())
                    .collect();
                if !failed.is_empty() {
                    self.paths = failed;
                    self.results.clear();
                    self.run_summary = None;
                    upload::start_upload(self, context);
                }
            }
            Message::RemoveCompletedFromQueue => {
                // Prune queue entries whose files the last run uploaded
                // or skipped (already in library / unsupported), then
                // dismiss the completion panel (spec §4.6). Queued
                // folders stay: results carry the individual files,
                // not the folder they were expanded from.
                let completed: Vec<PathBuf> = self
                    .results
                    .iter()
                    .filter(|row| {
                        row.outcome == "Uploaded" || row.outcome.starts_with("Skipped")
                    })
                    .map(|row| row.path.clone())
                    .collect();
                self.paths.retain(|path| !completed.contains(path));
                self.run_summary = None;
            }
            Message::OpenGooglePhotos => {
                // Same dependency-free browser launch as OpenSignInPage.
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", "https://photos.google.com/"])
                    .spawn();
            }

            // ---- Upload section ----
            Message::PathDraftChanged(value) => self.path_draft = value,
            Message::AddPathDraft => {
                let draft = self.path_draft.trim().to_string();
                if !draft.is_empty() {
                    let path = PathBuf::from(draft);
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                    self.path_draft.clear();
                }
            }
            Message::AddFiles => {
                let accepted = OpenFilePicker::new()
                    .title("Choose files to upload")
                    .filter_all()
                    .request_multiple(context, |result| match result {
                        Ok(paths) => Message::PathsPicked(paths),
                        Err(error) => Message::PickerFailed(error.to_string()),
                    });
                if !accepted {
                    self.upload_note =
                        Some(Note::error("The file picker could not be opened."));
                }
            }
            Message::AddFolders => {
                let accepted = FolderPicker::new()
                    .title("Choose folders to upload")
                    .request_multiple(context, |result| match result {
                        Ok(paths) => Message::PathsPicked(paths),
                        Err(error) => Message::PickerFailed(error.to_string()),
                    });
                if !accepted {
                    self.upload_note =
                        Some(Note::error("The folder picker could not be opened."));
                }
            }
            Message::PathsPicked(paths) => {
                for path in paths {
                    if !self.paths.contains(&path) {
                        self.paths.push(path);
                    }
                }
            }
            Message::PickerFailed(error) => {
                self.upload_note = Some(Note::error(format!("Picker failed: {error}")));
            }
            Message::RemovePath(index) => {
                if index < self.paths.len() {
                    self.paths.remove(index);
                }
            }
            Message::ClearPaths => {
                // Clearing the queue is destructive enough to confirm
                // (spec §2); the Upload page's Clear button will send
                // RequestClearQueue once it is redesigned — until then
                // the legacy message routes to the same dialog.
                self.confirm = Some(Confirm::ClearQueue);
            }
            Message::AlbumModeChanged(index) => {
                self.album_mode = AlbumMode::from_index(index);
            }
            Message::AlbumNameChanged(value) => self.album_name = value,
            Message::StartUpload => upload::start_upload(self, context),
            Message::CancelUpload => {
                if let Some(token) = &self.cancel_token {
                    token.cancel();
                }
                self.cancel_requested = true;
                self.upload_note = Some(Note::info("Cancelling…"));
            }

            // ---- Upload run (reporter bridge) ----
            Message::UploadStarted(total_files) => {
                self.running = true;
                self.total_files = total_files;
            }
            Message::UploadStopped => {
                self.running = false;
            }
            Message::UploadTotalBytes(total) => self.total_bytes = total,
            Message::UploadTotalBytesDelta(delta) => {
                let adjusted = self.total_bytes as i128 + delta as i128;
                self.total_bytes = adjusted.max(0) as u64;
            }
            Message::UploadWarning(warning) => {
                self.warnings.push(format!(
                    "{} — {}",
                    warning.file_path.display(),
                    warning.message
                ));
                if self.warnings.len() > 200 {
                    self.warnings.remove(0);
                }
            }
            Message::UploadThreadStatus(status) => {
                let row = WorkerRow {
                    worker: status.worker,
                    stage: upload::stage_label(&status.stage).to_string(),
                    file: upload::file_label(&status.file_path),
                    bytes_uploaded: status.bytes_uploaded,
                    bytes_total: status.bytes_total,
                    attempt: status.attempt,
                };
                match self
                    .workers
                    .iter_mut()
                    .find(|existing| existing.worker == row.worker)
                {
                    Some(existing) => *existing = row,
                    None => {
                        self.workers.push(row);
                        self.workers.sort_by_key(|existing| existing.worker);
                    }
                }
            }
            Message::UploadFileResult(result) => {
                let outcome = match result.outcome {
                    Outcome::Uploaded => "Uploaded",
                    Outcome::SkippedAlreadyPresent => "Skipped — already in library",
                    Outcome::SkippedUnsupported => "Skipped — unsupported file",
                    Outcome::Failed => "Failed",
                };
                self.results.push(ResultRow {
                    file: upload::file_label(&result.file_path),
                    path: result.file_path.clone(),
                    outcome: outcome.to_string(),
                    message: result.message,
                });
                if self.results.len() > 1_000 {
                    self.results.remove(0);
                }
            }
            Message::UploadAlbumProgress(status) => {
                self.album_status = Some(format!(
                    "Album '{}': {} of {} added",
                    status.name, status.done, status.total
                ));
            }
            Message::UploadAlbumComplete(status) => {
                self.album_status = Some(format!(
                    "Album '{}' complete: {} of {} added",
                    status.name, status.done, status.total
                ));
            }
            Message::UploadAlbumError(name, message) => {
                self.album_status = Some(format!("Album '{name}' failed: {message}"));
            }
            Message::UploadRunEnded(ended) => {
                self.running = false;
                self.cancel_token = None;
                // Worker rows describe a live run only; the completion
                // panel (spec §4.6) renders from `run_summary` instead.
                self.workers.clear();
                self.run_summary = Some(ended);
                self.upload_note = Some(if ended.cancelled {
                    Note::info("Upload cancelled.")
                } else {
                    Note::success(format!(
                        "Upload finished: {} uploaded · {} skipped · {} failed.",
                        ended.uploaded, ended.skipped, ended.failed
                    ))
                });
                self.cancel_requested = false;
            }

            // ---- Accounts section ----
            Message::OAuthTokenChanged(value) => self.oauth_token = value,
            Message::RawCredentialChanged(value) => self.raw_credential = value,
            Message::OpenSignInPage => {
                // Dependency-free browser launch (Windows shell). The
                // sign-in itself happens in the browser, never in the app.
                let _ = std::process::Command::new("cmd")
                    .args(["/C", "start", "", accounts::EMBEDDED_SETUP_URL])
                    .spawn();
                self.account_note = Some(Note::info(
                    "Sign-in page opened in your browser. Copy the oauth_token cookie value \
                     from DevTools and paste it here.",
                ));
            }
            Message::ConnectAccount => accounts::connect_account(self, context),
            Message::AccountConnected(Ok((email, credential, needs_token_binding))) => {
                self.auth_busy = false;
                self.oauth_token.clear();
                self.add_account(Account {
                    email,
                    credential,
                    needs_token_binding,
                });
            }
            Message::AccountConnected(Err(error)) => {
                self.auth_busy = false;
                self.account_note = Some(Note::error(format!("Sign-in failed: {error}")));
            }
            Message::ImportRawCredential => {
                let raw = self.raw_credential.trim().to_string();
                match Credential::parse(&raw) {
                    Ok(credential) => {
                        let account = Account {
                            email: credential.email().to_string(),
                            credential: raw,
                            needs_token_binding: credential.needs_token_binding(),
                        };
                        self.raw_credential.clear();
                        self.add_account(account);
                    }
                    Err(error) => {
                        self.account_note = Some(Note::error(format!(
                            "That does not look like a credential: {error}"
                        )));
                    }
                }
            }
            Message::SetActiveAccount(email) => match &mut self.store {
                Some(service) => match store::set_active(service, &email) {
                    Ok(()) => {
                        self.refresh_from_store();
                        self.account_note =
                            Some(Note::success(format!("{email} is now the active account.")));
                    }
                    Err(error) => {
                        self.account_note =
                            Some(Note::error(format!("Could not switch account: {error}")));
                    }
                },
                None => {
                    self.account_note = Some(Note::error(
                        "Config could not be loaded; accounts are unavailable.",
                    ));
                }
            },
            Message::RemoveAccount(email) => {
                // Removal is destructive and hard to reverse (F-27), so
                // the legacy message now routes through the confirmation
                // dialog exactly like RequestRemoveAccount (spec §2);
                // `remove_account_now` runs on ConfirmResolved(true).
                self.confirm = Some(Confirm::RemoveAccount(email));
            }

            // ---- Settings section ----
            Message::PrefBoolChanged(field, value) => {
                // Enabling delete-originals is one of the three
                // irreversible decisions reserved for a dialog (R-9):
                // route through the confirmation instead of applying.
                // Turning it OFF applies immediately.
                if field == PrefBool::DeleteFromHost && value && !self.prefs.delete_from_host {
                    self.confirm = Some(Confirm::DeleteOriginals);
                    return;
                }
                field.apply(&mut self.prefs, value);
                self.persist_preferences();
            }
            Message::PrefThreadsChanged(value) => {
                if let Some(value) = value {
                    if value.is_finite() {
                        self.prefs.upload_threads = value.round().clamp(1.0, 16.0) as u32;
                        self.persist_preferences();
                    }
                }
            }
            Message::PrefProxyChanged(value) => {
                self.prefs.proxy = value;
                self.persist_preferences();
            }
            Message::PrefExcludePatternChanged(value) => {
                self.prefs.exclude_pattern = value;
                self.persist_preferences();
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("Outh");
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Mica)
                .client_size(1100.0, 720.0)
                .constraints(WindowConstraints {
                    min_width: Some(640.0),
                    min_height: Some(480.0),
                    max_width: None,
                    max_height: None,
                }),
        );

        let section_view = match self.section {
            Section::Upload => upload::view(self, context),
            Section::Accounts => accounts::view(self, context),
            Section::Settings => settings::view(self, context),
        };
        // The config-load failure is the app's worst state (F-03): a
        // persistent, non-closable Error bar at the top of EVERY page
        // until the config loads — rendered once here, not per page.
        let content: View = match &self.store_error {
            Some(error) => Grid::new()
                .rows([GridLength::Auto, GridLength::STAR])
                .children(vec![
                    Border::new()
                        .padding(Thickness::new(
                            theme::PAGE_PADDING_X,
                            theme::SPACE_L,
                            theme::PAGE_PADDING_X,
                            0.0,
                        ))
                        .content(theme::info_bar(
                            NoteSeverity::Error,
                            Some("Config couldn't be loaded"),
                            error,
                            None,
                        ))
                        .into(),
                    // An erased View can't carry grid placement itself;
                    // wrap it (the same trick window_frame uses).
                    Border::new().grid_row(1).content(section_view).into(),
                ])
                .into(),
            None => section_view,
        };

        let item = |tag: &'static str,
                    label: &'static str,
                    symbol: Symbol,
                    selected: bool| {
            KeyedView::new(
                tag,
                NavigationViewItem::new()
                    .tag(tag)
                    .is_selected(selected)
                    .icon(Icon::symbol(symbol))
                    .content(label),
            )
        };
        let navigation = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::Auto)
            .is_settings_visible(false)
            .on_selected_tag_changed(context.callback(|tag: Option<Rc<str>>| {
                Message::NavigateTag(tag.map(|tag| tag.to_string()))
            }))
            .keyed_menu_items([
                item(
                    "upload",
                    "Upload",
                    Symbol::Upload,
                    self.section == Section::Upload,
                ),
                item(
                    "accounts",
                    "Accounts",
                    Symbol::People,
                    self.section == Section::Accounts,
                ),
            ])
            // The built-in settings item carries a null tag in WinUI, so
            // selection would arrive as `None` and never route; a footer
            // item with the standard gear icon stands in for it (the same
            // pattern the reactor gallery uses).
            .keyed_footer_menu_items([item(
                "settings",
                "Settings",
                Symbol::Setting,
                self.section == Section::Settings,
            )])
            .content(content)
            .grid_row(1);

        // Hand-composed frame (window_frame hard-codes its TitleBar and
        // shows a dead back chevron — I-28): app icon + title, no back
        // button, and NavigationView no longer repeats "Outh" as a pane
        // header (V-09).
        let title_bar = TitleBar::new()
            .title("Outh")
            .icon(Icon::symbol(Symbol::Pictures))
            .is_back_button_visible(false)
            .is_pane_toggle_button_visible(false);

        let frame: View = Grid::new()
            .rows([GridLength::Auto, GridLength::STAR])
            .children((title_bar, navigation))
            .into();

        // The one global confirmation dialog (spec §2), always attached
        // per the gallery pattern; `is_open` follows `self.confirm`.
        frame.content_dialog(confirm_dialog(self.confirm.as_ref(), context))
    }
}

/// Builds the root ContentDialog for the pending `Confirm` (closed and
/// empty when there is none). Primary = the destructive action;
/// dismissal (close button / Esc) resolves as `ConfirmResolved(false)`.
fn confirm_dialog(confirm: Option<&Confirm>, context: &mut ViewContext<OuthApp>) -> ContentDialog {
    let (title, body, primary): (&str, String, &str) = match confirm {
        Some(Confirm::RemoveAccount(email)) => (
            "Remove account?",
            format!(
                "Outh will forget {email}'s sign-in. To re-add it you would need to sign \
                 in again in your browser and paste a fresh oauth_token cookie. If this \
                 is the active account, uploads will need another account before they \
                 can run."
            ),
            "Remove account",
        ),
        Some(Confirm::ClearQueue) => (
            "Clear the queue?",
            "All queued files and folders will be removed from the list. \
             The files themselves stay on disk."
                .to_string(),
            "Clear queue",
        ),
        Some(Confirm::DeleteOriginals) => (
            "Delete originals after upload?",
            "With this on, Outh permanently deletes each local file once Google \
             confirms the upload — including files that turn out to already be in \
             your library, which are also deleted locally even though nothing is \
             uploaded for them. Deletion is permanent."
                .to_string(),
            "Delete originals",
        ),
        None => ("", String::new(), ""),
    };
    ContentDialog::new()
        .title(title)
        .content(
            TextBlock::new()
                .text(body)
                .text_wrapping(TextWrapping::Wrap),
        )
        .primary_button_text(primary)
        .close_button_text("Cancel")
        .is_open(confirm.is_some())
        .on_closed(
            context.callback(|result: ContentDialogResult| {
                Message::ConfirmResolved(matches!(result, ContentDialogResult::Primary))
            }),
        )
}

fn main() {
    App::run_component::<OuthApp>(()).unwrap();
}
